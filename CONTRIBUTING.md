# Contributing

Thanks for wanting to help!

1. Build the app with `setup.bat` (see the README).
2. Keep changes focused: one fix or feature per pull request.
3. Run `cargo fmt` and make sure `cargo build --release` passes without new warnings.
4. UI strings go through `crate::i18n::t("…")`. The key is the Russian text; add the
   Ukrainian and English translations to the table in `src/i18n.rs`.
5. Describe what you changed and, for UI changes, attach a screenshot.

Bug reports are welcome too. If the app crashed, please attach `%LOCALAPPDATA%\SCloud\crash.log`.
