//! What the load test prints: the budget of `docs/architecture.md` against what the bots measured.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use mmo_store::keys::{TICK_BUCKET, tick_bucket};

use crate::bot::Outcome;
use crate::layers::Seen;
use crate::ticks::Counts;

/// The target of the tick of a zone writer at the 99th percentile, and its hard limit.
const BUDGET: Duration = Duration::from_millis(25);
const LIMIT: Duration = Duration::from_millis(50);

/// The targets for one client: the data down, mean and at the 99th percentile, in bytes each
/// second, and the data up.
const DOWN_MEAN: f64 = 5_000.0;
const DOWN_WORST: f64 = 20_000.0;
const UP_MEAN: f64 = 2_000.0;

/// The most bytes of a datagram.
const DATAGRAM: usize = 1100;

/// The mean, the median, the 99th percentile and the most of some numbers.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Stats {
  count: usize,
  mean: f64,
  p50: f64,
  p99: f64,
  max: f64,
}

/// The number `n` as a float, which only a count that no run reaches could round.
fn count(n: usize) -> f64 {
  f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

impl Stats {
  pub(crate) fn new(mut values: Vec<f64>) -> Self {
    values.sort_by(f64::total_cmp);
    let at = |percent: usize| values[(values.len() * percent).div_ceil(100).max(1) - 1];
    match values.len() {
      0 => Self::default(),
      n => Self {
        count: n,
        mean: values.iter().sum::<f64>() / count(n),
        p50: at(50),
        p99: at(99),
        max: values[n - 1],
      },
    }
  }
}

/// The ticks of the writers: their count, the time that each percentile of them took, and how many
/// passed the budget and the limit.
#[derive(Debug, PartialEq)]
pub(crate) struct Ticks {
  total: u64,
  p50: Duration,
  p99: Duration,
  max: Duration,
  slow: u64,
  late: u64,
}

impl Ticks {
  /// A tick in bucket `n` took less than `n + 1` buckets, so a percentile is the end of its bucket.
  pub(crate) fn new(counts: &Counts) -> Self {
    let total: u64 = counts.values().sum();
    let end = |bucket: u32| TICK_BUCKET * (bucket + 1);
    let at = |percent: u64| {
      let rank = (total * percent).div_ceil(100).max(1);
      let mut seen = 0;
      let found = counts.iter().find(|(_, count)| {
        seen += **count;
        seen >= rank
      });
      found.map_or(Duration::ZERO, |(bucket, _)| end(*bucket))
    };
    let above = |limit: Duration| counts.range(tick_bucket(limit)..).map(|(_, count)| count).sum();
    Self {
      total,
      p50: at(50),
      p99: at(99),
      max: counts.keys().next_back().map_or(Duration::ZERO, |bucket| end(*bucket)),
      slow: above(BUDGET),
      late: above(LIMIT),
    }
  }
}

/// The result of a run.
#[derive(Debug)]
pub(crate) struct Report {
  bots: usize,
  played: usize,
  faults: BTreeMap<String, usize>,
  position: Stats,
  waited: Stats,
  entered: Stats,
  down: Stats,
  up: Stats,
  largest: usize,
  snapshots: u64,
  gaps: u64,
  ticks: Ticks,
  layers: Seen,
  /// The bots that played, by their shard.
  shards: BTreeMap<u32, usize>,
  window: Duration,
}

/// The least count of layers that hold `players` players, `cap` in each.
fn needed(players: usize, cap: u32) -> usize {
  match (players, cap) {
    (0, _) => 0,
    (_, 0) => usize::MAX,
    _ => players.div_ceil(cap as usize),
  }
}

impl Report {
  /// The report of the `outcomes` of `bots` bots, the `ticks` of the writers during `window`, and
  /// the `layers` that the directory showed. A bot that gave no outcome failed.
  pub(crate) fn new(
    bots: usize,
    outcomes: &[Outcome],
    ticks: &Counts,
    layers: Seen,
    window: Duration,
  ) -> Self {
    let seconds = Duration::as_secs_f64;
    let rate = |bytes: fn(&Outcome) -> usize| {
      let rates = outcomes.iter().filter(|out| !out.played.is_zero());
      Stats::new(rates.map(|out| count(bytes(out)) / seconds(&out.played)).collect())
    };
    let mut faults = BTreeMap::new();
    for fault in outcomes.iter().filter_map(|out| out.fault.clone()) {
      *faults.entry(fault).or_default() += 1;
    }
    let mut shards = BTreeMap::new();
    for out in outcomes.iter().filter(|out| out.joined.is_some()) {
      *shards.entry(out.shard).or_default() += 1;
    }
    Self {
      bots,
      played: outcomes.iter().filter(|out| out.joined.is_some()).count(),
      faults,
      position: Stats::new(
        outcomes.iter().filter_map(|out| out.place).map(|p| f64::from(p.position)).collect(),
      ),
      waited: Stats::new(
        outcomes.iter().filter_map(|out| out.waited).map(|d| seconds(&d)).collect(),
      ),
      entered: Stats::new(
        outcomes.iter().filter_map(|out| out.entered).map(|d| seconds(&d)).collect(),
      ),
      down: rate(|out| out.down),
      up: rate(|out| out.up),
      largest: outcomes.iter().map(|out| out.largest).max().unwrap_or_default(),
      snapshots: outcomes.iter().map(|out| out.snapshots).sum(),
      gaps: outcomes.iter().map(|out| out.gaps).sum(),
      ticks: Ticks::new(ticks),
      layers,
      shards,
      window,
    }
  }
}

fn verdict(holds: bool) -> &'static str {
  if holds { "ok" } else { "over" }
}

fn stats(label: &str, stats: &Stats, unit: &str) -> String {
  let Stats { count, mean, p50, p99, max } = stats;
  format!(
    "{label} ({unit}, {count} bots): mean {mean:.1}, p50 {p50:.1}, p99 {p99:.1}, max {max:.1}"
  )
}

fn millis(d: Duration) -> f64 {
  d.as_secs_f64() * 1000.0
}

impl fmt::Display for Report {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let mut lines = vec![format!(
      "load test: {} bots, {} played, {} failed",
      self.bots,
      self.played,
      self.bots - self.played
    )];
    lines.extend(self.faults.iter().map(|(fault, bots)| format!("  {bots} bots: {fault}")));
    lines.extend([
      stats("place in the queue at login", &self.position, "place"),
      stats("wait from login to session", &self.waited, "s"),
      stats("wait from login to the first snapshot", &self.entered, "s"),
      stats("data down for one client", &self.down, "B/s"),
      stats("data up for one client", &self.up, "B/s"),
    ]);
    let Ticks { total, p50, p99, max, slow, late } = &self.ticks;
    lines.push(format!(
      "snapshots {}, refused by the world {}, largest {} bytes",
      self.snapshots, self.gaps, self.largest
    ));
    lines.push(format!(
      "ticks of the writers in {:.0} s: {total}, p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms, {slow} over 25 ms, {late} over 50 ms",
      self.window.as_secs_f64(),
      millis(*p50),
      millis(*p99),
      millis(*max),
    ));
    for (id, shard) in &self.layers {
      let players: Vec<String> = shard.layers.values().map(|layer| layer.now.to_string()).collect();
      lines.push(format!(
        "shard {id}: {} layers of cap {} opened, {} bots played, players on each layer: {}",
        shard.layers.len(),
        shard.cap,
        self.shards.get(id).copied().unwrap_or_default(),
        players.join(" ")
      ));
    }
    let capped =
      self.layers.values().all(|shard| shard.layers.values().all(|l| l.most <= shard.cap));
    let opened = self.layers.iter().all(|(id, shard)| {
      shard.layers.len() >= needed(self.shards.get(id).copied().unwrap_or_default(), shard.cap)
    });
    let budget = [
      ("tick p99 at most 25 ms", *total > 0 && *p99 <= BUDGET),
      ("no tick of 50 ms or more", *total > 0 && *late == 0),
      ("data down, mean at most 5000 B/s", self.down.mean <= DOWN_MEAN),
      ("data down, p99 at most 20000 B/s", self.down.p99 <= DOWN_WORST),
      ("data up, mean at most 2000 B/s", self.up.mean <= UP_MEAN),
      ("no datagram over 1100 bytes", self.largest <= DATAGRAM),
      ("each bot played", self.played == self.bots),
      ("no layer over its cap", capped),
      ("each shard opened the layers that its players need", opened),
    ];
    lines.extend(budget.map(|(label, holds)| format!("budget {label}: {}", verdict(holds))));
    writeln!(f, "{}", lines.join("\n"))
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;
  use std::time::Duration;

  use mmo_client::Place;
  use tokio::time::Instant;

  use super::{Report, Stats, Ticks};
  use crate::bot::Outcome;
  use crate::layers::{Players, Seen, Shard};

  fn played(wait: u64, enter: u64, up: usize, down: usize, largest: usize) -> Outcome {
    Outcome {
      place: Some(Place { position: 2, length: 3, wait: 0 }),
      waited: Some(Duration::from_secs(wait)),
      entered: Some(Duration::from_secs(enter)),
      joined: Some(Instant::now()),
      shard: 1,
      played: Duration::from_secs(10),
      up,
      down,
      snapshots: 100,
      gaps: 1,
      largest,
      ..Outcome::default()
    }
  }

  /// A shard of cap `cap`, with the layers `(number, players now, most players)`.
  fn shard(cap: u32, layers: &[(u32, u32, u32)]) -> Shard {
    let layers = layers.iter().map(|&(number, now, most)| (number, Players { now, most }));
    Shard { cap, layers: layers.collect() }
  }

  fn failed(fault: &str) -> Outcome {
    Outcome { fault: Some(fault.to_owned()), ..Outcome::default() }
  }

  #[test]
  fn the_stats_of_some_numbers_are_their_mean_and_their_nearest_ranks() {
    assert_eq!(Stats::new(Vec::new()), Stats::default());
    let stats = Stats::new((1..=200).map(f64::from).collect());
    assert_eq!(
      (stats.count, stats.mean, stats.p50, stats.p99, stats.max),
      (200, 100.5, 100.0, 198.0, 200.0)
    );
    let one = Stats::new(vec![7.0]);
    assert_eq!((one.mean, one.p50, one.p99, one.max), (7.0, 7.0, 7.0, 7.0));
  }

  #[test]
  fn the_ticks_give_each_percentile_as_the_end_of_its_bucket_and_count_the_slow_ones() {
    let counts = BTreeMap::from([(10, 90), (60, 9), (120, 1)]);
    assert_eq!(
      Ticks::new(&counts),
      Ticks {
        total: 100,
        p50: Duration::from_micros(2750),
        p99: Duration::from_micros(15_250),
        max: Duration::from_micros(30_250),
        slow: 1,
        late: 0,
      }
    );
    let none = Ticks::new(&BTreeMap::new());
    assert_eq!(
      (none.total, none.p99, none.max, none.slow, none.late),
      (0, Duration::ZERO, Duration::ZERO, 0, 0)
    );
    assert_eq!(Ticks::new(&BTreeMap::from([(399, 2)])).late, 2);
  }

  #[test]
  fn the_report_prints_each_measure_and_says_which_target_it_misses() {
    let outcomes = [
      played(1, 2, 20_000, 40_000, 800),
      played(3, 4, 10_000, 60_000, 900),
      failed("the link fails"),
    ];
    let counts = BTreeMap::from([(10, 90), (60, 9), (120, 1)]);
    let seen = Seen::from([(1, shard(1, &[(1, 1, 1), (2, 1, 2)]))]);
    let report = Report::new(3, &outcomes, &counts, seen, Duration::from_secs(30));
    let lines = [
      "load test: 3 bots, 2 played, 1 failed",
      "  1 bots: the link fails",
      "place in the queue at login (place, 2 bots): mean 2.0, p50 2.0, p99 2.0, max 2.0",
      "wait from login to session (s, 2 bots): mean 2.0, p50 1.0, p99 3.0, max 3.0",
      "wait from login to the first snapshot (s, 2 bots): mean 3.0, p50 2.0, p99 4.0, max 4.0",
      "data down for one client (B/s, 2 bots): mean 5000.0, p50 4000.0, p99 6000.0, max 6000.0",
      "data up for one client (B/s, 2 bots): mean 1500.0, p50 1000.0, p99 2000.0, max 2000.0",
      "snapshots 200, refused by the world 2, largest 900 bytes",
      "ticks of the writers in 30 s: 100, p50 2.75 ms, p99 15.25 ms, max 30.25 ms, 1 over 25 ms, 0 over 50 ms",
      "shard 1: 2 layers of cap 1 opened, 2 bots played, players on each layer: 1 1",
      "budget tick p99 at most 25 ms: ok",
      "budget no tick of 50 ms or more: ok",
      "budget data down, mean at most 5000 B/s: ok",
      "budget data down, p99 at most 20000 B/s: ok",
      "budget data up, mean at most 2000 B/s: ok",
      "budget no datagram over 1100 bytes: ok",
      "budget each bot played: over",
      "budget no layer over its cap: over",
      "budget each shard opened the layers that its players need: ok",
    ];
    assert_eq!(report.to_string(), lines.join("\n") + "\n");
    let empty = Report::new(0, &[], &BTreeMap::new(), Seen::new(), Duration::from_secs(1));
    let text = empty.to_string();
    assert!(text.starts_with("load test: 0 bots, 0 played, 0 failed\n"), "{text}");
    assert!(
      text.contains("budget tick p99 at most 25 ms: over\nbudget no tick of 50 ms or more: over\n")
    );
  }

  #[test]
  fn the_report_fails_a_layer_over_its_cap_and_a_shard_that_opens_too_few_layers() {
    let outcomes = [played(1, 2, 1, 1, 1), played(1, 2, 1, 1, 1)];
    let text = |shard| {
      let seen = Seen::from([(1, shard)]);
      Report::new(2, &outcomes, &BTreeMap::new(), seen, Duration::from_secs(1)).to_string()
    };
    let capped = "budget no layer over its cap: ";
    let opened = "budget each shard opened the layers that its players need: ";
    let fits = text(shard(2, &[(1, 2, 2)]));
    assert!(fits.contains(&format!("{capped}ok\n{opened}ok")), "{fits}");
    let few = text(shard(1, &[(1, 1, 1)]));
    assert!(few.contains(&format!("{capped}ok\n{opened}over")), "{few}");
    let none = text(Shard::default());
    assert!(none.contains(&format!("{capped}ok\n{opened}over")), "{none}");
    let over = text(shard(1, &[(1, 1, 1), (2, 2, 2)]));
    assert!(over.contains(&format!("{capped}over\n{opened}ok")), "{over}");
  }
}
