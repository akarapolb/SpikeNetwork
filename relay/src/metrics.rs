//! Metrics a relay operator can read without being able to read anyone's messages (ADR 0022).
//!
//! Everything here is a number. There is no device id, address, recipient or content in any
//! name or label: only fixed names and a short fixed list of error codes. The text format is
//! the Prometheus exposition format, which most monitoring tools understand.

use crate::store::Stats;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// The error codes the relay can answer with (anything else is counted as `other`).
const ERROR_CODES: [&str; 19] = [
    "bad_request",
    "too_large",
    "queue_full",
    "rate_limited",
    "bad_prekey",
    "bad_list",
    "not_member",
    "rollback",
    "equivocation",
    "quota_exceeded",
    "not_owner",
    "auth_failed",
    "bad_signature",
    "unknown_mailbox",
    "sealed",
    "exists",
    "mailbox_full",
    "internal",
    "other",
];

#[derive(Default)]
pub struct Metrics {
    pub connections_current: AtomicI64,
    pub connections_total: AtomicU64,
    pub auth_ok: AtomicU64,
    pub auth_failed: AtomicU64,
    pub refused_too_many_connections: AtomicU64,
    pub refused_too_fast: AtomicU64,
    pub rate_limited_requests: AtomicU64,
    pub messages_accepted: AtomicU64,
    pub bytes_accepted: AtomicU64,
    pub delivered_live: AtomicU64,
    pub delivered_backlog: AtomicU64,
    pub acks: AtomicU64,
    pub prekey_claims: AtomicU64,
    pub prekeys_published: AtomicU64,
    pub device_lists_published: AtomicU64,
    pub chunks_stored: AtomicU64,
    pub chunks_served: AtomicU64,
    pub connections_evicted: AtomicU64,
    pub connections_idle_closed: AtomicU64,
    pub group_mailboxes_created: AtomicU64,
    pub group_appends: AtomicU64,
    pub group_bytes_appended: AtomicU64,
    pub group_pushes: AtomicU64,
    pub group_reads: AtomicU64,
    pub key_packages_published: AtomicU64,
    pub key_package_claims: AtomicU64,
    errors: [AtomicU64; ERROR_CODES.len()],
}

impl Metrics {
    pub fn inc(c: &AtomicU64) {
        c.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add(c: &AtomicU64, n: u64) {
        c.fetch_add(n, Ordering::Relaxed);
    }

    /// Count an error reply by its code.
    pub fn error(&self, code: &str) {
        let i = ERROR_CODES
            .iter()
            .position(|c| *c == code)
            .unwrap_or(ERROR_CODES.len() - 1);
        self.errors[i].fetch_add(1, Ordering::Relaxed);
    }

    /// Render everything in the Prometheus text format. `live_devices` is how many devices
    /// currently hold a connection; `stats` is what the database holds (None if it did not answer).
    pub fn render(&self, uptime_secs: u64, live_devices: usize, stats: Option<&Stats>) -> String {
        let mut out = String::new();
        let mut counter = |name: &str, help: &str, v: u64| {
            out.push_str(&format!("# HELP spike_relay_{name} {help}\n# TYPE spike_relay_{name} counter\nspike_relay_{name} {v}\n"));
        };
        let g = |c: &AtomicU64| c.load(Ordering::Relaxed);
        counter(
            "connections_total",
            "Connections accepted.",
            g(&self.connections_total),
        );
        counter("auth_ok_total", "Devices that signed in.", g(&self.auth_ok));
        counter(
            "auth_failed_total",
            "Sign-ins refused.",
            g(&self.auth_failed),
        );
        counter(
            "refused_too_many_connections_total",
            "Connections refused: too many open from one address.",
            g(&self.refused_too_many_connections),
        );
        counter(
            "refused_too_fast_total",
            "Connections refused: too many new ones from one address.",
            g(&self.refused_too_fast),
        );
        counter(
            "rate_limited_requests_total",
            "Requests refused because a connection asked too fast.",
            g(&self.rate_limited_requests),
        );
        counter(
            "messages_accepted_total",
            "Messages queued for a recipient.",
            g(&self.messages_accepted),
        );
        counter(
            "bytes_accepted_total",
            "Bytes of message ciphertext queued.",
            g(&self.bytes_accepted),
        );
        counter(
            "delivered_live_total",
            "Messages pushed to a connected device.",
            g(&self.delivered_live),
        );
        counter(
            "delivered_backlog_total",
            "Queued messages sent when a device connected.",
            g(&self.delivered_backlog),
        );
        counter(
            "acks_total",
            "Messages acknowledged by their recipient.",
            g(&self.acks),
        );
        counter(
            "prekey_claims_total",
            "Prekeys handed to senders.",
            g(&self.prekey_claims),
        );
        counter(
            "prekeys_published_total",
            "Prekeys uploaded.",
            g(&self.prekeys_published),
        );
        counter(
            "device_lists_published_total",
            "Signed device lists stored.",
            g(&self.device_lists_published),
        );
        counter(
            "chunks_stored_total",
            "Attachment chunks stored.",
            g(&self.chunks_stored),
        );
        counter(
            "chunks_served_total",
            "Attachment chunks fetched.",
            g(&self.chunks_served),
        );
        counter(
            "connections_evicted_total",
            "Connections closed because a device opened more than allowed.",
            g(&self.connections_evicted),
        );
        counter(
            "connections_idle_closed_total",
            "Connections closed after going quiet.",
            g(&self.connections_idle_closed),
        );
        counter(
            "group_mailboxes_created_total",
            "Group mailboxes registered (including by a seal).",
            g(&self.group_mailboxes_created),
        );
        counter(
            "group_appends_total",
            "Entries appended to group mailboxes.",
            g(&self.group_appends),
        );
        counter(
            "group_bytes_appended_total",
            "Bytes of group ciphertext appended.",
            g(&self.group_bytes_appended),
        );
        counter(
            "group_pushes_total",
            "Group entries pushed to subscribers.",
            g(&self.group_pushes),
        );
        counter(
            "group_reads_total",
            "Group mailbox reads answered.",
            g(&self.group_reads),
        );
        counter(
            "key_packages_published_total",
            "Key packages uploaded.",
            g(&self.key_packages_published),
        );
        counter(
            "key_package_claims_total",
            "Key packages handed to someone adding a device to a group.",
            g(&self.key_package_claims),
        );
        out.push_str("# HELP spike_relay_errors_total Error replies by code.\n# TYPE spike_relay_errors_total counter\n");
        for (code, n) in ERROR_CODES.iter().zip(&self.errors) {
            out.push_str(&format!(
                "spike_relay_errors_total{{code=\"{code}\"}} {}\n",
                n.load(Ordering::Relaxed)
            ));
        }
        let mut gauge = |name: &str, help: &str, v: i64| {
            out.push_str(&format!("# HELP spike_relay_{name} {help}\n# TYPE spike_relay_{name} gauge\nspike_relay_{name} {v}\n"));
        };
        gauge(
            "uptime_seconds",
            "Seconds since the relay started.",
            uptime_secs as i64,
        );
        gauge(
            "connections_current",
            "Connections open now.",
            self.connections_current.load(Ordering::Relaxed),
        );
        gauge(
            "devices_connected",
            "Devices with at least one connection.",
            live_devices as i64,
        );
        match stats {
            Some(s) => {
                gauge("db_up", "1 if the database answered.", 1);
                gauge(
                    "queued_messages",
                    "Messages waiting for a recipient.",
                    s.queued_messages as i64,
                );
                gauge(
                    "queued_bytes",
                    "Bytes of queued ciphertext.",
                    s.queued_bytes as i64,
                );
                gauge(
                    "blob_chunks",
                    "Attachment chunks stored.",
                    s.blob_chunks as i64,
                );
                gauge(
                    "blob_bytes",
                    "Bytes of stored attachment chunks.",
                    s.blob_bytes as i64,
                );
                gauge("prekeys", "Prekeys held for devices.", s.prekeys as i64);
                gauge(
                    "device_lists",
                    "Signed device lists held.",
                    s.device_lists as i64,
                );
                gauge(
                    "group_mailboxes",
                    "Group mailboxes held.",
                    s.group_mailboxes as i64,
                );
                gauge(
                    "group_entries",
                    "Entries held in group mailboxes.",
                    s.group_entries as i64,
                );
                gauge(
                    "group_bytes",
                    "Bytes of group ciphertext held.",
                    s.group_bytes as i64,
                );
                gauge(
                    "key_packages",
                    "One-time key packages held for devices.",
                    s.key_packages as i64,
                );
            }
            None => gauge("db_up", "1 if the database answered.", 0),
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendering_is_numbers_only_and_counts_what_happened() {
        let m = Metrics::default();
        Metrics::inc(&m.messages_accepted);
        Metrics::add(&m.bytes_accepted, 1234);
        m.error("queue_full");
        m.error("queue_full");
        m.error("something_new"); // unknown codes are counted, not lost
        m.connections_current.fetch_add(2, Ordering::Relaxed);
        let stats = Stats {
            queued_messages: 7,
            ..Stats::default()
        };
        let text = m.render(42, 3, Some(&stats));
        for line in [
            "spike_relay_messages_accepted_total 1",
            "spike_relay_bytes_accepted_total 1234",
            "spike_relay_errors_total{code=\"queue_full\"} 2",
            "spike_relay_errors_total{code=\"other\"} 1",
            "spike_relay_connections_current 2",
            "spike_relay_devices_connected 3",
            "spike_relay_uptime_seconds 42",
            "spike_relay_queued_messages 7",
            "spike_relay_db_up 1",
        ] {
            assert!(text.lines().any(|l| l == line), "missing {line}\n{text}");
        }
        // Every sample line is `name{labels} number`: nothing else can leak into it.
        for l in text.lines().filter(|l| !l.starts_with('#')) {
            let (name, value) = l.rsplit_once(' ').unwrap();
            assert!(value.parse::<i64>().is_ok(), "{l}");
            assert!(name.starts_with("spike_relay_"), "{l}");
        }
        assert!(m.render(0, 0, None).contains("spike_relay_db_up 0"));
    }
}
