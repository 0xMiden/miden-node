use std::time::Duration;

/// Prints a summary of the benchmark results, including the average and 95th percentile
/// request latencies.
pub fn print_summary(timers_accumulator: Vec<Duration>) {
    let avg_time = timers_accumulator.iter().sum::<Duration>() / timers_accumulator.len() as u32;
    println!("Average request latency: {avg_time:?}");

    let p95_time = compute_percentile(timers_accumulator, 95);
    println!("P95 request latency: {p95_time:?}");
}

/// Computes a percentile from a list of durations.
fn compute_percentile(mut times: Vec<Duration>, percentile: u32) -> Duration {
    if times.is_empty() {
        return Duration::ZERO;
    }

    times.sort_unstable();

    let index = (percentile as usize * times.len()).div_ceil(100).saturating_sub(1);
    times[index.min(times.len() - 1)]
}
