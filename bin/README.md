# Local browser test binaries

This directory is reserved for repo-local browser test helpers such as a
ChromeDriver binary that matches the installed Chrome version.

If `wasm-pack test --headless --chrome` is blocked by a Homebrew ChromeDriver
version mismatch, place the matching `chromedriver` binary here and run the
test with this directory first on `PATH`.
