//! A throwaway Summonr home for core tests, driven through the RPC dispatcher.
use super::*;
use std::path::Path;

pub(super) struct Home {
    pub(super) core: Arc<Core>,
    pub(super) root: PathBuf,
}

impl Home {
    pub(super) fn new(config: &str) -> Self {
        let root = std::env::temp_dir().join(new_id("summonr-safe"));
        let paths = Paths {
            runtime: root.join("run"),
            state: root.join("state"),
            config: root.join("config"),
        };
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(paths.config.join("config.yaml"), config).unwrap();
        let core = Core::open(paths).unwrap();
        Home { core, root }
    }

    /// A home whose mailbox holds one receipt in Inbox, with inbox-triage
    /// inscribed and granted.
    pub(super) fn triage() -> (Self, PathBuf) {
        let root = std::env::temp_dir().join(new_id("summonr-safe-mail"));
        let mailbox = root.join("mail");
        for f in ["Inbox", "Receipts"] {
            std::fs::create_dir_all(mailbox.join(f)).unwrap();
        }
        std::fs::write(
            mailbox.join("Inbox/001.eml"),
            "From: shop@example.com\nSubject: your receipt\n\nYour receipt: total 12.00",
        )
        .unwrap();
        let h = Home::new(&format!("mail_root: \"{}\"\n", mailbox.display()));
        h.inscribe(&repo("tests/fixtures/rituals/classify-message.yaml"), false);
        h.inscribe(&repo("tests/fixtures/rituals/inbox-triage.yaml"), false);
        h.core
            .dispatch(
                "grant",
                json!({"ritual":"inbox-triage","scopes":[{"reagent":"mail","all":true}],"on":{"folder":"Inbox"}}),
            )
            .unwrap();
        h.core
            .dispatch(
                "grant",
                json!({"ritual":"inbox-triage","scopes":[{"reagent":"mail","select":["locate"]}]}),
            )
            .unwrap();
        (h, root)
    }

    pub(super) fn inscribe(&self, path: &Path, replace: bool) {
        self.core
            .dispatch(
                "inscribe",
                json!({"path": path.to_str().unwrap(), "replace": replace}),
            )
            .unwrap();
    }

    pub(super) fn inscribe_def(&self, def: Value, replace: bool) {
        let path = self.root.join(format!("{}.json", new_id("r")));
        std::fs::write(&path, def.to_string()).unwrap();
        self.inscribe(&path, replace);
    }

    pub(super) fn run(&self, params: Value) -> Value {
        let started = self.core.dispatch("invoke", params).unwrap();
        self.core
            .dispatch(
                "wait",
                json!({"summon": started["summon"], "timeout_ms": 20000}),
            )
            .unwrap()
    }

    /// Stop this core and open a new one on the same home, as after a crash
    /// or a restart. Waits for the old core's threads to let go of it.
    pub(super) fn restart(mut self) -> Home {
        self.core.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        for _ in 0..500 {
            if Arc::strong_count(&self.core) == 1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(Arc::strong_count(&self.core), 1, "the old core is still in use");
        let paths = self.core.paths.clone();
        let root = std::mem::take(&mut self.root);
        drop(self);
        // The old core's lock and database handles close as it drops; give a
        // loaded machine a moment before calling it a failure.
        let mut last = String::new();
        for _ in 0..100 {
            match Core::open(paths.clone()) {
                Ok(core) => return Home { core, root },
                Err(e) => last = e,
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("cannot reopen the core: {last}");
    }

    pub(super) fn call(&self, method: &str, params: Value) -> Result<Value, RpcErr> {
        self.core.dispatch(method, params)
    }

    pub(super) fn petitions(&self) -> i64 {
        self.core
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM petitions", [], |r| r.get(0))
            .unwrap()
    }

    pub(super) fn path(&self, name: &str) -> String {
        self.root.join(name).to_str().unwrap().to_string()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if self.root.as_os_str().is_empty() {
            return;
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub(super) fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

