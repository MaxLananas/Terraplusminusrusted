Inputs that once crashed a decoder, found by `tests/fuzz_decoders.rs` (run deeper with
`TERRA_FUZZ_SCALE=<n>`, and keep failing inputs with `TERRA_FUZZ_DUMP=<directory>`).
`tests/regressions.rs` feeds every file here to every decoder; each must return without
panicking.

| File | Bug |
|---|---|
| `tiff_argb_inverted_gray_wrapped_sample.tif` | MinIsWhite with a predictor-wrapped sample: `255 - sample` overflowed (`tiff_rgb.rs`); Java `int` arithmetic wraps |
