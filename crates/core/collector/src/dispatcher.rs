// marius-collector · dispatcher.rs
// Boucle principale du moteur réactif.
// Réveil sur tick temporel ou seuil volumétrique (Notify).
// Adaptive tick : contrôleur bang-bang (ADR reactive-projection).

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio::time::interval;
use rayon::prelude::*;

use crate::collector::Collector;
use crate::projection::Projection;

pub struct DispatcherConfig {
    pub tick_default:    Duration,
    pub tick_min:        Duration,
    pub tick_max:        Duration,
    pub threshold_flush: usize,
    pub threshold_low:   usize,
    pub threshold_high:  usize,
    pub render_budget:   Duration,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            tick_default:    Duration::from_millis(500),
            tick_min:        Duration::from_millis(100),
            tick_max:        Duration::from_millis(2_000),
            threshold_flush: 128,
            threshold_low:   10,
            threshold_high:  100,
            render_budget:   Duration::from_millis(200),
        }
    }
}

pub struct Dispatcher<P: Projection, const MAX: usize, const WORDS: usize> {
    collector: &'static Collector<MAX, WORDS>,
    notify:    Arc<Notify>,
    pool:      sqlx::PgPool,
    config:    DispatcherConfig,
    _phantom:  std::marker::PhantomData<P>,
}

impl<P: Projection, const MAX: usize, const WORDS: usize> Dispatcher<P, MAX, WORDS> {
    pub fn new(
        collector: &'static Collector<MAX, WORDS>,
        notify:    Arc<Notify>,
        pool:      sqlx::PgPool,
        config:    DispatcherConfig,
    ) -> Self {
        Self { collector, notify, pool, config, _phantom: std::marker::PhantomData }
    }

    pub async fn run(self) {
        let mut current_tick = self.config.tick_default;
        let mut ticker       = interval(current_tick);

        loop {
            tokio::select! {
                _ = ticker.tick()          => {}
                _ = self.notify.notified() => {}
            }

            let ids = self.collector.flush();
            if ids.is_empty() { continue; }

            let t0      = Instant::now();
            let records = match P::fetch_batch(&self.pool, &ids).await {
                Ok(r)  => r,
                Err(e) => { eprintln!("Dispatcher fetch_batch: {e}"); continue; }
            };

            // into_par_iter() consomme le Vec<Record> — requiert T: Send uniquement.
            // par_iter() requiert T: Sync, bound que le compilateur ne propage pas
            // toujours depuis le trait associé dans les generics profonds.
            records.into_par_iter().for_each(|record| {
                let html = P::render(&record);
                let path = P::artifact_path(&record);
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::write(&path, html) {
                    eprintln!("Dispatcher write {:?}: {e}", path);
                }
            });

            let new_tick = self.adapt_tick(ids.len(), t0.elapsed());
            if new_tick != current_tick {
                current_tick = new_tick;
                // interval() Tokio ne supporte pas le changement de période à chaud.
                ticker = interval(current_tick);
            }
        }
    }

    fn adapt_tick(&self, batch_size: usize, elapsed: Duration) -> Duration {
        let pressure = elapsed > self.config.render_budget
                    || batch_size > self.config.threshold_high;
        let quiet    = batch_size < self.config.threshold_low;
        match (pressure, quiet) {
            (true, _) => self.config.tick_max,
            (_, true) => self.config.tick_min,
            _         => self.config.tick_default,
        }
    }
}
