# Evaluation suite prerequisites

Before running the eval suite, set up:

## Tier 1 prerequisites

1. **Discord** installed and logged in. Create a private text channel
   `#fastuse-eval-channel` in any server you control.
2. **VS Code** on PATH (`code` command works).
3. **Google Chrome** at default install path
   (`C:\Program Files\Google\Chrome\Application\chrome.exe`).
4. **L-Connect3** installed (Tier 2 scenarios reference it; skip those if absent).

## Tier 2 prerequisites

5. **L-Connect3** installed and visible on screen.
6. **GIMP** installed (or Photoshop, if you have a license — adapt scenario).
7. **Steam** installed and signed in.
8. **A test app you compiled.** Set:
   - `FASTUSE_TEST_APP_PATH` to its `.exe` path
   - `FASTUSE_TEST_APP_TITLE` to a substring of its window title
   - `FASTUSE_TEST_APP_SUCCESS_NEEDLE` to a string visible after the primary
     interaction succeeds

## Running the suite

Tier 1 scenarios:

    .\target\release\eval.exe run-tier 1

Tier 2 scenarios:

    .\target\release\eval.exe run-tier 2
