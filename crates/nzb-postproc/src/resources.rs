//! Resource gates shared by independent post-processing jobs.
//!
//! The limits are deliberately expressed as worker counts. PAR2 work is the
//! CPU- and memory-heavy stage while extraction is primarily constrained by
//! disk throughput. Bounding each stage separately lets one job extract while
//! another repairs without allowing either workload to grow without bound.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Concurrent post-processing worker limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostProcLimits {
    /// Jobs admitted to the post-processing pipeline at once.
    pub pipelines: usize,
    /// Concurrent PAR2 verify/repair workers.
    pub repair: usize,
    /// Concurrent archive extraction workers.
    pub extract: usize,
}

impl Default for PostProcLimits {
    fn default() -> Self {
        Self {
            pipelines: 2,
            repair: 1,
            extract: 1,
        }
    }
}

impl PostProcLimits {
    fn normalized(self) -> Self {
        Self {
            pipelines: self.pipelines.max(1),
            repair: self.repair.max(1),
            extract: self.extract.max(1),
        }
    }
}

/// Current and lifetime-high concurrent stage usage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PostProcResourceSnapshot {
    pub active_pipelines: usize,
    pub active_repairs: usize,
    pub active_extractions: usize,
    pub peak_pipelines: usize,
    pub peak_repairs: usize,
    pub peak_extractions: usize,
}

#[derive(Debug, Default)]
struct Usage {
    active: AtomicUsize,
    peak: AtomicUsize,
}

impl Usage {
    fn acquired(&self) {
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak.fetch_max(active, Ordering::Relaxed);
    }

    fn released(&self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Shared stage-specific semaphore set for the post-processing pipeline.
#[derive(Debug)]
pub struct PostProcResourcePool {
    limits: PostProcLimits,
    pipelines: Arc<Semaphore>,
    repairs: Arc<Semaphore>,
    extractions: Arc<Semaphore>,
    pipeline_usage: Arc<Usage>,
    repair_usage: Arc<Usage>,
    extraction_usage: Arc<Usage>,
}

impl PostProcResourcePool {
    pub fn new(limits: PostProcLimits) -> Arc<Self> {
        let limits = limits.normalized();
        Arc::new(Self {
            limits,
            pipelines: Arc::new(Semaphore::new(limits.pipelines)),
            repairs: Arc::new(Semaphore::new(limits.repair)),
            extractions: Arc::new(Semaphore::new(limits.extract)),
            pipeline_usage: Arc::new(Usage::default()),
            repair_usage: Arc::new(Usage::default()),
            extraction_usage: Arc::new(Usage::default()),
        })
    }

    pub fn limits(&self) -> PostProcLimits {
        self.limits
    }

    pub async fn acquire_pipeline(self: &Arc<Self>) -> PostProcPermit {
        Self::acquire(&self.pipelines, &self.pipeline_usage).await
    }

    pub(crate) async fn acquire_repair(self: &Arc<Self>) -> PostProcPermit {
        Self::acquire(&self.repairs, &self.repair_usage).await
    }

    pub(crate) async fn acquire_extract(self: &Arc<Self>) -> PostProcPermit {
        Self::acquire(&self.extractions, &self.extraction_usage).await
    }

    async fn acquire(semaphore: &Arc<Semaphore>, usage: &Arc<Usage>) -> PostProcPermit {
        let permit = Arc::clone(semaphore)
            .acquire_owned()
            .await
            .expect("post-processing resource semaphore must remain open");
        usage.acquired();
        PostProcPermit {
            _permit: permit,
            usage: Arc::clone(usage),
        }
    }

    pub fn snapshot(&self) -> PostProcResourceSnapshot {
        PostProcResourceSnapshot {
            active_pipelines: self.pipeline_usage.active.load(Ordering::Relaxed),
            active_repairs: self.repair_usage.active.load(Ordering::Relaxed),
            active_extractions: self.extraction_usage.active.load(Ordering::Relaxed),
            peak_pipelines: self.pipeline_usage.peak.load(Ordering::Relaxed),
            peak_repairs: self.repair_usage.peak.load(Ordering::Relaxed),
            peak_extractions: self.extraction_usage.peak.load(Ordering::Relaxed),
        }
    }
}

/// RAII permit which releases both the semaphore slot and usage counter.
#[derive(Debug)]
pub struct PostProcPermit {
    _permit: OwnedSemaphorePermit,
    usage: Arc<Usage>,
}

impl Drop for PostProcPermit {
    fn drop(&mut self) {
        self.usage.released();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn different_stages_can_overlap_but_same_stage_respects_its_limit() {
        let pool = PostProcResourcePool::new(PostProcLimits {
            pipelines: 2,
            repair: 1,
            extract: 1,
        });
        let _pipeline_a = pool.acquire_pipeline().await;
        let _pipeline_b = pool.acquire_pipeline().await;
        let repair_a = pool.acquire_repair().await;
        let _extract_b = pool.acquire_extract().await;

        let overlap = pool.snapshot();
        assert_eq!(overlap.active_pipelines, 2);
        assert_eq!(overlap.active_repairs, 1);
        assert_eq!(overlap.active_extractions, 1);

        let waiting_pool = Arc::clone(&pool);
        let mut waiting_repair = tokio::spawn(async move { waiting_pool.acquire_repair().await });
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut waiting_repair)
                .await
                .is_err(),
            "a second repair must wait for the configured repair worker"
        );

        drop(repair_a);
        let repair_b = tokio::time::timeout(Duration::from_secs(1), waiting_repair)
            .await
            .expect("waiting repair should be admitted")
            .expect("waiting repair task should not panic");
        drop(repair_b);

        let snapshot = pool.snapshot();
        assert_eq!(snapshot.peak_pipelines, 2);
        assert_eq!(snapshot.peak_repairs, 1);
        assert_eq!(snapshot.peak_extractions, 1);
    }

    #[tokio::test]
    async fn zero_limits_are_safely_normalized_to_one_worker() {
        let pool = PostProcResourcePool::new(PostProcLimits {
            pipelines: 0,
            repair: 0,
            extract: 0,
        });
        assert_eq!(
            pool.limits(),
            PostProcLimits {
                pipelines: 1,
                repair: 1,
                extract: 1,
            }
        );
    }

    #[tokio::test]
    async fn stress_many_jobs_never_exceeds_stage_worker_limits() {
        let pool = PostProcResourcePool::new(PostProcLimits {
            pipelines: 3,
            repair: 2,
            extract: 1,
        });
        let mut jobs = Vec::new();
        for index in 0..24 {
            let resources = Arc::clone(&pool);
            jobs.push(tokio::spawn(async move {
                let _pipeline = resources.acquire_pipeline().await;
                if index % 2 == 0 {
                    let _repair = resources.acquire_repair().await;
                    tokio::task::yield_now().await;
                }
                let _extract = resources.acquire_extract().await;
                tokio::task::yield_now().await;
            }));
        }
        for job in jobs {
            job.await.expect("stress job should not panic");
        }

        let snapshot = pool.snapshot();
        assert!(snapshot.peak_pipelines <= 3, "{snapshot:?}");
        assert!(snapshot.peak_repairs <= 2, "{snapshot:?}");
        assert!(snapshot.peak_extractions <= 1, "{snapshot:?}");
        assert_eq!(snapshot.active_pipelines, 0);
        assert_eq!(snapshot.active_repairs, 0);
        assert_eq!(snapshot.active_extractions, 0);
    }
}
