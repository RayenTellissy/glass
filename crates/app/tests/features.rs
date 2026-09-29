//! Each `app` feature must reach the crate that owns it.
//! Run with `--features release-full` to check that every flag resolves.

#[test]
fn app_features_reach_the_engine() {
    let enabled = engine::enabled_features();
    let expected = [
        ("codec-av1", cfg!(feature = "codec-av1")),
        ("codec-prores-sw", cfg!(feature = "codec-prores-sw")),
        ("ai-whisper", cfg!(feature = "ai-whisper")),
        ("ai-segmentation", cfg!(feature = "ai-segmentation")),
    ];
    for (name, on) in expected {
        assert_eq!(enabled.contains(&name), on, "feature {name}");
    }
    if cfg!(feature = "release-full") {
        assert_eq!(enabled.len(), expected.len());
    }
}
