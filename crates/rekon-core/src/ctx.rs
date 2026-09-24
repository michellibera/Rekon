//! Shared context of one repository: root, store, config, backend and cost meter.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use anyhow::{Result, anyhow};

use crate::backend::{self, Backend, LlmRequest, LlmResponse};
use crate::config::Config;
use crate::prompts;
use crate::scan;
use crate::store::Store;

/// Sum of `total_cost_usd` in this process, in micro-dollars.
#[derive(Default)]
pub struct CostMeter(AtomicU64);

impl CostMeter {
    pub fn add(&self, usd: f64) {
        self.0.fetch_add((usd * 1_000_000.0).round() as u64, Ordering::Relaxed);
    }

    pub fn usd(&self) -> f64 {
        self.0.load(Ordering::Relaxed) as f64 / 1_000_000.0
    }
}

pub struct Ctx {
    pub root: PathBuf,
    pub store: Store,
    pub config: Config,
    pub cost: CostMeter,
    backend: OnceLock<std::result::Result<Arc<dyn Backend>, String>>,
}

impl Ctx {
    /// Finds the repository containing `start` and loads its config.
    pub fn open(start: &Path) -> Result<Self> {
        let root = scan::find_root(start)?;
        Self::at_root(&root)
    }

    pub fn at_root(root: &Path) -> Result<Self> {
        let store = Store::new(root);
        let config = Config::load(store.dir())?;
        Ok(Self {
            root: root.to_path_buf(),
            store,
            config,
            cost: CostMeter::default(),
            backend: OnceLock::new(),
        })
    }

    /// Context with a given backend (tests, alternative frontends).
    pub fn with_backend(root: &Path, config: Config, backend: Arc<dyn Backend>) -> Self {
        let ctx = Self {
            root: root.to_path_buf(),
            store: Store::new(root),
            config,
            cost: CostMeter::default(),
            backend: OnceLock::new(),
        };
        let _ = ctx.backend.set(Ok(backend));
        ctx
    }

    /// The configured backend, created on first use.
    pub fn backend(&self) -> Result<Arc<dyn Backend>> {
        self.backend
            .get_or_init(|| backend::from_config(&self.config).map_err(|e| e.to_string()))
            .clone()
            .map_err(|e| anyhow!(e))
    }

    pub fn system_prompt(&self) -> String {
        prompts::system(&self.config.language, &self.store.style())
    }

    /// One model call with a single retry; every attempt is logged (without code).
    pub fn ask(&self, req: &LlmRequest) -> Result<LlmResponse> {
        let backend = self.backend()?;
        let mut last_err = None;
        for attempt in 1..=2 {
            let started = Instant::now();
            let result = backend.ask(req);
            let secs = started.elapsed().as_secs_f64();
            match result {
                Ok(resp) => {
                    let cost = resp.cost_usd.unwrap_or(0.0);
                    self.cost.add(cost);
                    let tokens = resp.output_tokens.map_or(String::new(), |t| format!(" out_tokens={t}"));
                    self.store.log(&format!(
                        "call task={} model={} time={secs:.1}s cost=${cost:.4}{tokens} ok",
                        req.kind.name(),
                        req.model
                    ));
                    return Ok(resp);
                }
                Err(e) => {
                    self.store.log(&format!(
                        "call task={} model={} time={secs:.1}s attempt={attempt} error={e:#}",
                        req.kind.name(),
                        req.model
                    ));
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.expect("at least one attempt"))
    }
}
