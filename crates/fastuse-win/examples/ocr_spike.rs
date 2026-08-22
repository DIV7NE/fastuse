//! Throwaway spike: measure Windows.Media.Ocr accuracy on real windows before
//! deciding whether a `find_text` tool can be built on it.
//!
//!   cargo run -p fastuse-win --example ocr_spike -- <image.png> [needle]

use windows::Graphics::Imaging::BitmapDecoder;
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};

#[tokio::main(flavor = "current_thread")]
async fn main() -> windows::core::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("usage: ocr_spike <image> [needle]");
    let needle = args.get(2).cloned();

    let bytes = std::fs::read(path).expect("read image");

    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream)?;
    writer.WriteBytes(&bytes)?;
    writer.StoreAsync()?.await?;
    writer.FlushAsync()?.await?;
    writer.DetachStream()?;
    stream.Seek(0)?;

    let decoder = BitmapDecoder::CreateAsync(&stream)?.await?;
    let bitmap = decoder.GetSoftwareBitmapAsync()?.await?;

    let engine = OcrEngine::TryCreateFromUserProfileLanguages()?;
    println!(
        "engine max image dimension: {} px (image is {}x{})",
        OcrEngine::MaxImageDimension()?,
        decoder.PixelWidth()?,
        decoder.PixelHeight()?
    );
    let started = std::time::Instant::now();
    let result = engine.RecognizeAsync(&bitmap)?.await?;
    let elapsed = started.elapsed();

    let lines = result.Lines()?;
    let mut word_count = 0;
    println!("--- OCR of {path} ---");
    for line in &lines {
        let text = line.Text()?.to_string();
        word_count += line.Words()?.Size()?;
        println!("  {text}");
    }
    println!(
        "--- {} lines, {} words, {:?} ---",
        lines.Size()?,
        word_count,
        elapsed
    );

    if let Some(needle) = needle {
        let lower = needle.to_lowercase();
        let mut hits = 0;
        for line in &lines {
            for word in &line.Words()? {
                if word.Text()?.to_string().to_lowercase().contains(&lower) {
                    let r = word.BoundingRect()?;
                    println!(
                        "  HIT {:?} at x={} y={} w={} h={} -> click ({}, {})",
                        word.Text()?.to_string(),
                        r.X, r.Y, r.Width, r.Height,
                        r.X + r.Width / 2.0,
                        r.Y + r.Height / 2.0
                    );
                    hits += 1;
                }
            }
        }
        println!("--- {hits} hit(s) for {needle:?} ---");
    }
    Ok(())
}
