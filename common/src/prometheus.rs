use std::fmt::Write as _;

use crate::metrics::HistogramSnapshot;

pub fn push_counter(output: &mut String, name: &str, help: &str, service: &str, value: u64) {
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} counter");
    let _ = writeln!(output, "{name}{{service=\"{service}\"}} {value}");
}

pub fn push_gauge(output: &mut String, name: &str, help: &str, service: &str, value: u64) {
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} gauge");
    let _ = writeln!(output, "{name}{{service=\"{service}\"}} {value}");
}

pub fn push_histogram(
    output: &mut String,
    name: &str,
    help: &str,
    service: &str,
    extra_labels: &[(&str, &str)],
    snapshot: &HistogramSnapshot,
) {
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} histogram");
    push_histogram_samples(output, name, service, extra_labels, snapshot);
}

pub fn push_histogram_header(output: &mut String, name: &str, help: &str) {
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} histogram");
}

pub fn push_histogram_samples(
    output: &mut String,
    name: &str,
    service: &str,
    extra_labels: &[(&str, &str)],
    snapshot: &HistogramSnapshot,
) {
    for (bucket_ms, count) in snapshot
        .buckets_ms
        .iter()
        .zip(snapshot.cumulative_counts.iter())
    {
        let labels = prometheus_labels(service, extra_labels, Some(*bucket_ms));
        let _ = writeln!(output, "{name}_bucket{{{labels}}} {count}");
    }
    let labels = prometheus_labels(service, extra_labels, None);
    let _ = writeln!(
        output,
        "{name}_bucket{{{labels},le=\"+Inf\"}} {}",
        snapshot.count
    );
    let _ = writeln!(output, "{name}_sum{{{labels}}} {}", snapshot.sum_ms);
    let _ = writeln!(output, "{name}_count{{{labels}}} {}", snapshot.count);
}

fn prometheus_labels(service: &str, extra_labels: &[(&str, &str)], le: Option<u64>) -> String {
    let mut labels = format!("service=\"{}\"", escape_label_value(service));
    for (name, value) in extra_labels {
        let _ = write!(labels, ",{name}=\"{}\"", escape_label_value(value));
    }
    if let Some(le) = le {
        let _ = write!(labels, ",le=\"{le}\"");
    }
    labels
}

fn escape_label_value(value: &str) -> String {
    value
        .replace('\\', r"\\")
        .replace('"', r#"\""#)
        .replace('\n', r"\n")
}
