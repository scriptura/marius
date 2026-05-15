// Dispatcher — boucle principale du moteur réactif.
// Réveil sur tick temporel ou seuil volumétrique (Notify).
// Adaptive tick : ajustement bang-bang selon charge (ADR reactive-projection).

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio::time::interval;

use crate::collector::Collector;
use crate::projection::Projection;

/// Configuration du Dispatcher.
pub struct DispatcherConfig {
    pub tick_default:    Duration,
    pub tick_min:        Duration,  // charge faible  → réactivité max
    pub tick_max:        Duration,  // charge élevée  → débit max
    pub threshold_flush: usize,     // seuil volumétrique → notify
    pub threshold_low:   usize,     // en dessous : tick_min
    pub threshold_high:  usize,     // au dessus  : tick_max
    pub render_budget:   Duration,  // durée de rendu au delà de laquelle on passe en tick_max
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

/// Dispatcher générique sur une seule Projection.
/// Pour plusieurs tables surveillées : instancier un Dispatcher par Projection
/// et les faire tourner en parallèle (tokio::spawn par table).
pub struct Dispatcher<P: Projection, const MAX: usize>
where
    [(); (MAX + 63) / 64]: Sized,
{
    collector: &'static Collector<MAX>,
    notify:    Arc<Notify>,
    pool:      sqlx::PgPool,
    config:    DispatcherConfig,
    _phantom:  std::marker::PhantomData<P>,
}

impl<P: Projection, const MAX: usize> Dispatcher<P, MAX>
where
    [(); (MAX + 63) / 64]: Sized,
{
    pub fn new(
        collector: &'static Collector<MAX>,
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
                _ = ticker.tick()            => {}
                _ = self.notify.notified()   => {}
            }

            let ids = self.collector.flush();
            if ids.is_empty() { continue; }

            let t0      = Instant::now();
            let records = match P::fetch_batch(&self.pool, &ids).await {
                Ok(r)  => r,
                Err(e) => {
                    eprintln!("Dispatcher fetch_batch error: {e}");
                    continue;
                }
            };

            // Projection parallèle via Rayon (CPU-bound).
            // render() et write_artifact() sont des opérations pures + I/O disque.
            rayon::scope(|s| {
                for record in &records {
                    s.spawn(|_| {
                        let html = P::render(record);
                        let path = P::artifact_path(record);
                        if let Err(e) = std::fs::write(&path, html) {
                            eprintln!("Dispatcher write error {:?}: {e}", path);
                        }
                    });
                }
            });

            let elapsed = t0.elapsed();

            // Adaptive tick — contrôleur bang-bang à deux seuils.
            let new_tick = self.adapt_tick(ids.len(), elapsed);
            if new_tick != current_tick {
                current_tick = new_tick;
                // interval() Tokio ne supporte pas le changement de période à chaud.
                // On recrée l'interval — coût négligeable (allocation unique).
                ticker = interval(current_tick);
            }
        }
    }

    fn adapt_tick(&self, batch_size: usize, render_time: Duration) -> Duration {
        let cpu_pressure = render_time > self.config.render_budget;
        let volume_high  = batch_size  > self.config.threshold_high;
        let volume_low   = batch_size  < self.config.threshold_low;

        match (cpu_pressure || volume_high, volume_low) {
            (true, _) => self.config.tick_max,
            (_, true) => self.config.tick_min,
            _         => self.config.tick_default,
        }
    }
}
