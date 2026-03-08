use httpcache::config::EnvMode;
use httpcache::server::prepare_runtime_dirs;
use serial_test::serial;
use std::sync::Mutex;

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn lock_test() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

fn clear_cache_dir() {
    let _ = std::fs::remove_dir_all(".cache");
}

#[tokio::test]
#[serial]
async fn dev_mode_clears_existing_cache_dir() {
    let _guard = lock_test();
    clear_cache_dir();

    std::fs::create_dir_all(".cache").unwrap();
    std::fs::write(".cache/sentinel.txt", b"dev").unwrap();

    prepare_runtime_dirs(EnvMode::Dev).await.unwrap();

    assert!(!std::path::Path::new(".cache/sentinel.txt").exists());
    assert!(!std::path::Path::new(".cache").exists());
}

#[tokio::test]
#[serial]
async fn prod_mode_preserves_existing_cache_dir() {
    let _guard = lock_test();
    clear_cache_dir();

    std::fs::create_dir_all(".cache").unwrap();
    std::fs::write(".cache/sentinel.txt", b"prod").unwrap();

    prepare_runtime_dirs(EnvMode::Prod).await.unwrap();

    assert!(std::path::Path::new(".cache/sentinel.txt").exists());

    clear_cache_dir();
}
