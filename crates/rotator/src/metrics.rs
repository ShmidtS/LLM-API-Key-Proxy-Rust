use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};

const BUCKETS: [u64; 8] = [50, 100, 250, 500, 1000, 2500, 5000, 10_000];

#[derive(Debug, Default)]
struct Histogram {
    buckets: [AtomicU64; 8],
    sum: AtomicU64,
    count: AtomicU64,
}

impl Histogram {
    fn record(&self, value: u64) {
        let mut bucket = BUCKETS.len() - 1;
        for (i, &b) in BUCKETS.iter().enumerate() {
            if value <= b {
                bucket = i;
                break;
            }
        }
        self.buckets[bucket].fetch_add(1, Ordering::Relaxed);
        self.sum.fetch_add(value, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    fn write(&self, name: &str, labels: &str, out: &mut String) {
        let count = self.count.load(Ordering::Relaxed);
        let sum = self.sum.load(Ordering::Relaxed);
        for (i, &b) in BUCKETS.iter().enumerate() {
            let v = self.buckets[i].load(Ordering::Relaxed);
            out.push_str(&format!("{name}_bucket{{{labels},le=\"{b}\"}} {v}\n"));
        }
        out.push_str(&format!("{name}_bucket{{{labels},le=\"+Inf\"}} {count}\n"));
        out.push_str(&format!("{name}_sum{{{labels}}} {sum}\n"));
        out.push_str(&format!("{name}_count{{{labels}}} {count}\n"));
    }
}

#[derive(Debug, Default)]
pub struct ProxyMetrics {
    request_dispatch_latency_ms: Histogram,
    request_duration_ms: Histogram,
    requests_total: DashMap<String, AtomicU64>,
    errors_total: DashMap<(String, String), AtomicU64>,
    retries_total: DashMap<String, AtomicU64>,
    concurrent_requests: AtomicU64,
    chunk_latency_ms: Histogram,
    stream_chunks_total: DashMap<String, AtomicU64>,
}

impl ProxyMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_request_dispatch_latency(&self, _provider: &str, duration_ms: u64) {
        self.request_dispatch_latency_ms.record(duration_ms);
    }

    pub fn record_request_duration(&self, provider: &str, duration_ms: u64) {
        self.request_duration_ms.record(duration_ms);
        self.requests_total
            .entry(provider.to_owned())
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_error(&self, provider: &str, error_class: &str) {
        self.errors_total
            .entry((provider.to_owned(), error_class.to_owned()))
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_retry(&self, provider: &str) {
        self.retries_total
            .entry(provider.to_owned())
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_stream_chunk(&self, provider: &str) {
        self.stream_chunks_total
            .entry(provider.to_owned())
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_first_chunk_latency(&self, _provider: &str, duration_ms: u64) {
        self.chunk_latency_ms.record(duration_ms);
    }

    pub fn inc_concurrent(&self) {
        self.concurrent_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_concurrent(&self) {
        self.concurrent_requests.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn export_prometheus(&self) -> String {
        let mut out = String::new();

        out.push_str("# TYPE proxy_request_dispatch_latency_ms histogram\n");
        self.request_dispatch_latency_ms
            .write("proxy_request_dispatch_latency_ms", "", &mut out);

        // request_duration_ms
        out.push_str("# TYPE proxy_request_duration_ms histogram\n");
        self.request_duration_ms
            .write("proxy_request_duration_ms", "", &mut out);

        // requests_total
        out.push_str("# TYPE proxy_requests_total counter\n");
        for entry in self.requests_total.iter() {
            let v = entry.value().load(Ordering::Relaxed);
            let provider = entry.key();
            out.push_str(&format!(
                "proxy_requests_total{{provider=\"{provider}\"}} {v}\n"
            ));
        }

        // errors_total
        out.push_str("# TYPE proxy_errors_total counter\n");
        for entry in self.errors_total.iter() {
            let v = entry.value().load(Ordering::Relaxed);
            let (provider, class) = entry.key();
            out.push_str(&format!(
                "proxy_errors_total{{provider=\"{provider}\",error_class=\"{class}\"}} {v}\n"
            ));
        }

        // retries_total
        out.push_str("# TYPE proxy_retries_total counter\n");
        for entry in self.retries_total.iter() {
            let v = entry.value().load(Ordering::Relaxed);
            let provider = entry.key();
            out.push_str(&format!(
                "proxy_retries_total{{provider=\"{provider}\"}} {v}\n"
            ));
        }

        // concurrent_requests
        out.push_str("# TYPE proxy_concurrent_requests gauge\n");
        out.push_str(&format!(
            "proxy_concurrent_requests {}\n",
            self.concurrent_requests.load(Ordering::Relaxed)
        ));

        // chunk_latency_ms
        out.push_str("# TYPE proxy_chunk_latency_ms histogram\n");
        self.chunk_latency_ms
            .write("proxy_chunk_latency_ms", "", &mut out);

        // stream_chunks_total
        out.push_str("# TYPE proxy_stream_chunks_total counter\n");
        for entry in self.stream_chunks_total.iter() {
            let v = entry.value().load(Ordering::Relaxed);
            let provider = entry.key();
            out.push_str(&format!(
                "proxy_stream_chunks_total{{provider=\"{provider}\"}} {v}\n"
            ));
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_records_into_correct_buckets() {
        let h = Histogram::default();
        h.record(30);
        h.record(75);
        h.record(150);
        h.record(500);
        h.record(5000);
        h.record(15000);

        assert_eq!(h.buckets[0].load(Ordering::Relaxed), 1); // <= 50
        assert_eq!(h.buckets[1].load(Ordering::Relaxed), 1); // <= 100
        assert_eq!(h.buckets[2].load(Ordering::Relaxed), 1); // <= 250
        assert_eq!(h.buckets[3].load(Ordering::Relaxed), 1); // <= 500
        assert_eq!(h.buckets[4].load(Ordering::Relaxed), 0);
        assert_eq!(h.buckets[5].load(Ordering::Relaxed), 0);
        assert_eq!(h.buckets[6].load(Ordering::Relaxed), 1); // <= 5000
        assert_eq!(h.buckets[7].load(Ordering::Relaxed), 1); // <= 10000
        assert_eq!(h.count.load(Ordering::Relaxed), 6);
        assert_eq!(
            h.sum.load(Ordering::Relaxed),
            30 + 75 + 150 + 500 + 5000 + 15000
        );
    }

    #[test]
    fn export_contains_all_metrics() {
        let m = ProxyMetrics::new();
        m.record_request_duration("openai", 123);
        m.record_error("openai", "rate_limit");
        m.record_retry("openai");
        m.record_stream_chunk("openai");
        m.record_first_chunk_latency("openai", 200);
        m.inc_concurrent();

        let out = m.export_prometheus();
        assert!(out.contains("proxy_request_duration_ms"));
        assert!(out.contains("proxy_requests_total{provider=\"openai\"}"));
        assert!(out.contains("proxy_errors_total{provider=\"openai\",error_class=\"rate_limit\"}"));
        assert!(out.contains("proxy_retries_total{provider=\"openai\"}"));
        assert!(out.contains("proxy_concurrent_requests 1"));
        assert!(out.contains("proxy_chunk_latency_ms"));
        assert!(out.contains("proxy_stream_chunks_total{provider=\"openai\"}"));
    }
}
