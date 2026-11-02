// SPDX-License-Identifier: GPL-3.0-or-later
//! Downloads against a mock Hub: an interrupted transfer resumes where it
//! stopped, a checksum mismatch is rejected, gated repositories ask for a
//! token, and the cache keeps `huggingface_hub`'s layout.

// The client needs TLS support compiled in, even for plain HTTP (D-065).
#![cfg(feature = "tls")]

mod common;

use common::{COMMIT, MockHub, git_oid, sha256};
use secrecy::SecretString;
use termos_models::cache::{Cache, Progress, PullError};
use termos_models::hub::{Hub, HubError};

fn content(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 % 251) as u8).collect()
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

#[test]
fn an_interrupted_download_resumes() {
    let hub_srv = MockHub::start();
    let model = content(300_000);
    hub_srv.add("org/tiny-GGUF", "tiny-Q4_K_M.gguf", &model);
    hub_srv.add("org/tiny-GGUF", "README.md", b"# tiny\n");
    hub_srv
        .state
        .lock()
        .expect("lock")
        .cut_after
        .insert("tiny-Q4_K_M.gguf".into(), 100_000);
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = Cache::new(dir.path().to_owned());
    let hub = Hub::at(&hub_srv.base, None).expect("hub");
    let rt = rt();
    let commit = rt
        .block_on(hub.commit("org/tiny-GGUF", "main"))
        .expect("commit");
    assert_eq!(commit, COMMIT);
    let files = rt
        .block_on(hub.files("org/tiny-GGUF", &commit))
        .expect("files");
    assert_eq!(files.len(), 2);
    let gguf = files
        .iter()
        .find(|f| f.path.ends_with(".gguf"))
        .expect("gguf");
    assert_eq!(gguf.sha256.as_deref(), Some(sha256(&model).as_str()));

    // The first try is cut off at 100 000 bytes; the part is kept.
    let mut last = Progress {
        done: 0,
        total: 0,
        resumed: 0,
    };
    let err = rt
        .block_on(cache.pull(&hub, "org/tiny-GGUF", &commit, gguf, &mut |p| last = p))
        .expect_err("cut off");
    assert!(err.to_string().contains("resume"), "{err}");
    let partial = cache
        .repo_dir("org/tiny-GGUF")
        .join("blobs")
        .join(format!("{}.incomplete", sha256(&model)));
    assert_eq!(
        std::fs::metadata(&partial).expect("part kept").len(),
        100_000
    );

    // The second asks for the rest only, and the result checks out.
    let path = rt
        .block_on(cache.pull(&hub, "org/tiny-GGUF", &commit, gguf, &mut |p| last = p))
        .expect("resumed");
    assert_eq!(last.resumed, 100_000);
    assert_eq!(last.done, 300_000);
    let log = hub_srv.state.lock().expect("lock").log.clone();
    assert!(
        log.iter()
            .any(|(p, r)| p.ends_with("tiny-Q4_K_M.gguf") && r.as_deref() == Some("bytes=100000-")),
        "{log:?}"
    );
    assert_eq!(std::fs::read(&path).expect("read"), model);
    assert!(!partial.exists());
    assert_eq!(
        path,
        cache
            .snapshot_dir("org/tiny-GGUF", COMMIT)
            .join("tiny-Q4_K_M.gguf")
    );
    #[cfg(unix)]
    assert_eq!(
        std::fs::read_link(&path).expect("a link"),
        std::path::PathBuf::from(format!("../../blobs/{}", sha256(&model)))
    );

    // A small file is checked against its git blob id.
    let readme = files
        .iter()
        .find(|f| f.path == "README.md")
        .expect("readme");
    assert_eq!(readme.git_oid, git_oid(b"# tiny\n"));
    let p = rt
        .block_on(cache.pull(&hub, "org/tiny-GGUF", &commit, readme, &mut |_| {}))
        .expect("readme");
    assert_eq!(std::fs::read(p).expect("read"), b"# tiny\n");

    // Pulling again downloads nothing.
    let before = hub_srv.state.lock().expect("lock").log.len();
    rt.block_on(cache.pull(&hub, "org/tiny-GGUF", &commit, gguf, &mut |_| {}))
        .expect("cached");
    assert_eq!(hub_srv.state.lock().expect("lock").log.len(), before);

    let repos = cache.repos();
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0].repo, "org/tiny-GGUF");
    assert_eq!(repos[0].snapshots[0].size(), 300_000 + 7);
}

#[test]
fn a_checksum_mismatch_is_rejected() {
    let hub_srv = MockHub::start();
    let model = content(50_000);
    hub_srv.add("org/bad", "bad-Q8_0.gguf", &model);
    hub_srv
        .state
        .lock()
        .expect("lock")
        .corrupt
        .push("bad-Q8_0.gguf".into());
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = Cache::new(dir.path().to_owned());
    let hub = Hub::at(&hub_srv.base, None).expect("hub");
    let rt = rt();
    let files = rt.block_on(hub.files("org/bad", COMMIT)).expect("files");
    let err = rt
        .block_on(cache.pull(&hub, "org/bad", COMMIT, &files[0], &mut |_| {}))
        .expect_err("mismatch");
    assert!(matches!(err, PullError::Checksum { .. }), "{err}");
    let blobs = cache.repo_dir("org/bad").join("blobs");
    assert_eq!(
        std::fs::read_dir(&blobs).expect("blobs").count(),
        0,
        "nothing kept"
    );
    assert!(
        !cache
            .snapshot_dir("org/bad", COMMIT)
            .join("bad-Q8_0.gguf")
            .exists()
    );
}

#[test]
fn gated_repositories_need_a_token_and_get_it() {
    let hub_srv = MockHub::start();
    hub_srv.add("org/gated", "m.gguf", &content(10));
    hub_srv.state.lock().expect("lock").token = Some("hf_test_token".into());
    let rt = rt();
    let anon = Hub::at(&hub_srv.base, None).expect("hub");
    let err = rt
        .block_on(anon.commit("org/gated", "main"))
        .expect_err("gated");
    assert!(matches!(err, HubError::Gated { .. }), "{err}");
    assert!(err.to_string().contains("HF_TOKEN"));
    let with = Hub::at(
        &hub_srv.base,
        Some(SecretString::from("hf_test_token".to_owned())),
    )
    .expect("hub");
    assert_eq!(
        rt.block_on(with.commit("org/gated", "main"))
            .expect("commit"),
        COMMIT
    );
    let err = rt
        .block_on(with.commit("org/missing", "main"))
        .expect_err("missing");
    assert!(matches!(err, HubError::NotFound { .. }), "{err}");
}

#[test]
fn removing_and_collecting() {
    let hub_srv = MockHub::start();
    hub_srv.add("org/a", "a-Q4_0.gguf", &content(4000));
    hub_srv.add("org/b", "b-Q4_0.gguf", &content(6000));
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = Cache::new(dir.path().to_owned());
    let hub = Hub::at(&hub_srv.base, None).expect("hub");
    let rt = rt();
    for repo in ["org/a", "org/b"] {
        let files = rt.block_on(hub.files(repo, COMMIT)).expect("files");
        rt.block_on(cache.pull(&hub, repo, COMMIT, &files[0], &mut |_| {}))
            .expect("pull");
    }
    // An orphan blob is collected.
    let orphan = cache.repo_dir("org/a").join("blobs").join("deadbeef");
    std::fs::write(&orphan, b"x").expect("orphan");
    let (_, removed) = cache.gc(None);
    assert!(
        removed.iter().any(|r| r.contains("deadbeef")),
        "{removed:?}"
    );
    assert!(!orphan.exists());
    // Over a size limit, the least recently used repository goes.
    termos_models::cache::touch(&cache.snapshot_dir("org/b", COMMIT));
    std::thread::sleep(std::time::Duration::from_millis(20));
    termos_models::cache::touch(&cache.snapshot_dir("org/a", COMMIT));
    let (_, removed) = cache.gc(Some(5000));
    assert_eq!(removed, ["org/b"]);
    assert!(cache.remove("org/a").expect("rm") > 0);
    assert!(cache.repos().is_empty());
}
