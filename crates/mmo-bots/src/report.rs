//! What the load test prints: the budget of `docs/architecture.md` against what the bots measured.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use mmo_protocol::Layer;
use mmo_store::keys::{TICK_BUCKET, tick_bucket};
use tokio::time::Instant;

use crate::bot::{End, Outcome, Role};
use crate::layers::{Players, Seen, Shard};
use crate::ticks::Counts;

/// The target of the tick of a layer writer at the 99th percentile, and its hard limit.
const BUDGET: Duration = Duration::from_millis(25);
const LIMIT: Duration = Duration::from_millis(50);

/// The targets for one client: the data down, mean and at the 99th percentile, in bytes each
/// second, and the data up.
const DOWN_MEAN: f64 = 5_000.0;
const DOWN_WORST: f64 = 20_000.0;
const UP_MEAN: f64 = 2_000.0;

/// The most bytes of a datagram.
const DATAGRAM: usize = 1100;

/// The longest time that a waiting bot may go without a place: the login service gives one each
/// 5 s.
const PLACE_GAP: Duration = Duration::from_secs(7);

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

/// What the queue did, from the bots that entered it.
#[derive(Debug, PartialEq)]
struct Queue {
  /// The bots that waited when the measure ended.
  waiting: usize,
  /// The bots that had to wait.
  expected: usize,
  /// No bot that got a session entered the queue after a bot that waits.
  ordered: bool,
  /// Each waiting bot, in the order of entry, holds the place of its rank, in a queue as long as
  /// the count of waiting bots.
  placed: bool,
  /// The longest time that a waiting bot went without a place, up to the end of the measure.
  gap: Duration,
}

impl Queue {
  /// The line of the report on the queue.
  fn line(&self) -> String {
    format!(
      "queue: {} bots wait of {} that must, the longest time without a place {:.1} s",
      self.waiting,
      self.expected,
      self.gap.as_secs_f64()
    )
  }

  /// The lines of the budget on the queue.
  fn budget(&self) -> [(&'static str, bool); 4] {
    [
      ("the queue holds the bots that must wait", self.waiting == self.expected),
      ("the queue admits in the order of entry", self.ordered),
      ("each waiting bot holds the place of its order of entry", self.placed),
      ("each waiting bot gets its place at least each 7 s", self.gap <= PLACE_GAP),
    ]
  }

  fn new(expected: usize, outcomes: &[Outcome]) -> Self {
    let mut entered: Vec<&Outcome> = outcomes.iter().filter(|out| !out.places.is_empty()).collect();
    entered.sort_by_key(|out| out.places.first().map(|(at, _)| *at));
    let waiting: Vec<&Outcome> = entered.iter().copied().filter(|out| out.queued).collect();
    let first = entered.iter().position(|out| out.queued).unwrap_or(entered.len());
    let ordered = entered.iter().skip(first).all(|out| out.waited.is_none());
    let placed = waiting.iter().enumerate().all(|(rank, out)| {
      out.places.last().is_some_and(|(_, place)| {
        place.position as usize == rank && place.length as usize == waiting.len()
      })
    });
    let gaps = waiting.iter().flat_map(|out| {
      let marks: Vec<Instant> = out.places.iter().map(|(at, _)| *at).chain(out.ended).collect();
      let pairs = marks.iter().zip(marks.iter().skip(1));
      pairs.map(|(from, to)| to.duration_since(*from)).collect::<Vec<_>>()
    });
    Self { waiting: waiting.len(), expected, ordered, placed, gap: gaps.max().unwrap_or_default() }
  }
}

/// What the bots that lost their link did: the longest time that one took to play again.
#[derive(Debug, PartialEq)]
struct Recovery {
  /// The longest time that the rule allows, or 0 when no bot attaches again.
  bound: Duration,
  /// The count of the losses of a link that ended with the player in play again.
  recovered: usize,
  /// The bots that lost their link and were not back when the measure ended.
  away: usize,
  /// The losses that ended on another layer.
  relocated: u32,
  /// The longest time without a snapshot of a loss that ended.
  gap: Duration,
}

impl Recovery {
  /// The line of the report on the bots that lost their link.
  fn line(&self) -> String {
    let Self { recovered, away, relocated, gap, .. } = self;
    format!(
      "recovery: {recovered} links came back, the longest time without a snapshot {:.1} s, {away} bots still away, {relocated} on another layer",
      gap.as_secs_f64()
    )
  }

  fn new(bound: Duration, outcomes: &[Outcome]) -> Self {
    let gaps = outcomes.iter().flat_map(|out| out.recoveries.iter().copied());
    Self {
      bound,
      recovered: outcomes.iter().map(|out| out.recoveries.len()).sum(),
      away: outcomes.iter().filter(|out| out.away).count(),
      relocated: outcomes.iter().map(|out| out.relocated).sum(),
      gap: gaps.max().unwrap_or_default(),
    }
  }

  /// The lines of the budget that a run with a bound for the recovery adds.
  fn budget(&self) -> [(String, bool); 3] {
    [
      ("each bot that lost its link played again".to_owned(), self.away == 0),
      (format!("each bot played again within {} s", self.bound.as_secs()), self.gap <= self.bound),
      ("each bot played again on its layer".to_owned(), self.relocated == 0),
    ]
  }
}

/// What the bots that cross the border of their zone did.
#[derive(Debug, PartialEq)]
struct Crossing {
  /// The bots that start near the east border and walk east.
  crossers: usize,
  /// The crossers that entered the zone after their own.
  crossed: usize,
  /// The moves of the bots to another layer, of each zone.
  moves: usize,
  /// The moves of the crossers into the next zone, and those of them that kept the number of their
  /// layer. A bot that does not cross may walk over the border, and it owes nothing to its number.
  entries: usize,
  kept: usize,
}

impl Crossing {
  fn new(outcomes: &[Outcome]) -> Self {
    let next = |(from, to): &&(Layer, Layer)| to.zone.0 == from.zone.0 + 1;
    let crossers = || outcomes.iter().filter(|out| out.role == Role::Crosser);
    let entries = || crossers().flat_map(|out| &out.crossings).filter(next);
    Self {
      crossers: crossers().count(),
      crossed: crossers().filter(|out| out.crossings.iter().any(|one| next(&one))).count(),
      moves: outcomes.iter().map(|out| out.crossings.len()).sum(),
      entries: entries().count(),
      kept: entries().filter(|(from, to)| to.number == from.number).count(),
    }
  }

  /// The line of the report, when some bots cross.
  fn line(&self) -> Option<String> {
    let Self { crossers, crossed, moves, entries, kept } = self;
    let text = format!(
      "crossings: {moves} moves to another layer, {crossed} of {crossers} crossers entered the next zone, {kept} of {entries} entries kept the number of their layer"
    );
    (*crossers > 0).then_some(text)
  }

  /// The lines of the budget, when some bots cross. A crossing keeps the number of its layer when
  /// the layer with that number is open in the next zone and has room: the stage that runs it
  /// holds each crosser of a shard on one layer, which the next zone opens with the first crosser.
  fn budget(&self) -> Vec<(&'static str, bool)> {
    let crossed = ("each crosser entered the next zone", self.crossed >= self.crossers);
    let kept =
      ("each entry into the next zone kept the number of its layer", self.kept == self.entries);
    if self.crossers > 0 { vec![crossed, kept] } else { Vec::new() }
  }
}

/// What the bots that log out left behind them: the bots that play on, and the layers of the last
/// look at the directory, which the drain of the layers that the leavers emptied made.
#[derive(Debug, PartialEq)]
struct Drain {
  /// The bots that must log out, and those that did.
  leavers: usize,
  left: usize,
  /// The bots that must play to the end, and those that play at the end.
  staying: usize,
  playing: usize,
  /// The layers that stand with no player at the last look.
  empty: usize,
  /// The layers that hold a quarter of their cap or fewer at the last look, while the other layers
  /// of their shard have room for their players.
  sparse: usize,
}

impl Drain {
  fn new(outcomes: &[Outcome], layers: &Seen) -> Self {
    let (mut empty, mut sparse) = (0, 0);
    for shard in layers.values() {
      let counts: Vec<u32> = shard.layers.values().filter_map(|layer| layer.now).collect();
      let room: u32 = counts.iter().map(|players| shard.cap.saturating_sub(*players)).sum();
      let fits = |players: u32| room - shard.cap.saturating_sub(players) >= players;
      empty += counts.iter().filter(|players| **players == 0).count();
      let low = |players: &&u32| **players > 0 && **players * 4 <= shard.cap;
      sparse += counts.iter().filter(low).filter(|players| fits(**players)).count();
    }
    let staying: Vec<&Outcome> = outcomes.iter().filter(|out| out.role != Role::Leaver).collect();
    Self {
      leavers: outcomes.len() - staying.len(),
      left: outcomes.iter().filter(|out| out.end == End::LoggedOut).count(),
      staying: staying.len(),
      playing: staying.iter().filter(|out| out.end == End::Playing).count(),
      empty,
      sparse,
    }
  }

  /// The line of the report, when some bots log out.
  fn line(&self) -> Option<String> {
    let Self { leavers, left, staying, playing, empty, sparse } = self;
    let text = format!(
      "drain: {left} of {leavers} bots logged out, {playing} of {staying} bots left play, {empty} layers stand empty, {sparse} sparse layers fit in the others"
    );
    (*leavers > 0).then_some(text)
  }

  /// The lines of the budget, when some bots log out.
  fn budget(&self) -> Option<[(&'static str, bool); 3]> {
    let plays = self.left == self.leavers && self.playing == self.staying;
    let lines = [
      ("each bot left plays", plays),
      ("no layer stands empty", self.empty == 0),
      ("no layer of a quarter of its cap or fewer fits in the other layers", self.sparse == 0),
    ];
    (self.leavers > 0).then_some(lines)
  }
}

/// The result of a run.
#[derive(Debug)]
pub(crate) struct Report {
  bots: usize,
  played: usize,
  queue: Queue,
  recovery: Recovery,
  crossing: Crossing,
  /// The bots of another shard that the bots saw, each counted once for each bot that saw it.
  strangers: usize,
  /// The bots that the end of the measure found before their first snapshot, by their phase: a
  /// bot that waits in the queue is one of them.
  unjoined: BTreeMap<&'static str, usize>,
  drain: Drain,
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
  /// The bots that played and never left their zone, by their shard: the players that its layers
  /// of the zone must hold.
  stayed: BTreeMap<u32, usize>,
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
  /// The report of the `outcomes` of `bots` bots, of which `waiting` must wait in the queue at the
  /// end, the `ticks` of the writers during `window`, and the `layers` that the directory showed.
  /// A bot that gave no outcome failed.
  pub(crate) fn new(
    bots: usize,
    waiting: usize,
    recover: Duration,
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
    let mut stayed = BTreeMap::new();
    let left = |out: &Outcome| out.crossings.iter().any(|(from, to)| from.zone != to.zone);
    for out in outcomes.iter().filter(|out| out.joined.is_some() && !left(out)) {
      *stayed.entry(out.shard).or_default() += 1;
    }
    let mut unjoined = BTreeMap::new();
    for out in outcomes.iter().filter(|out| out.joined.is_none() && out.fault.is_none()) {
      *unjoined.entry(out.phase).or_default() += 1;
    }
    Self {
      bots,
      played: outcomes.iter().filter(|out| out.joined.is_some()).count(),
      queue: Queue::new(waiting, outcomes),
      recovery: Recovery::new(recover, outcomes),
      crossing: Crossing::new(outcomes),
      strangers: outcomes.iter().map(|out| out.strangers.len()).sum(),
      unjoined,
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
      drain: Drain::new(outcomes, &layers),
      layers,
      shards,
      stayed,
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

impl Report {
  /// The line of the report on one shard: its layers and the players on each.
  fn shard_line(&self, id: u32, shard: &Shard) -> String {
    let now = |layer: &Players| layer.now.map_or("closed".to_owned(), |now| now.to_string());
    let players: Vec<String> = shard.layers.values().map(now).collect();
    format!(
      "shard {id}: {} layers of cap {} opened, {} bots played, players on each layer: {}",
      shard.layers.len(),
      shard.cap,
      self.shards.get(&id).copied().unwrap_or_default(),
      players.join(" ")
    )
  }
}

impl Report {
  /// Whether bots of more than one shard played, so that a bot could see a bot of another.
  fn sighted(&self) -> bool {
    self.shards.len() > 1
  }

  /// The line of the report on the bots of another shard that a bot saw, when two shards play.
  fn sight(&self) -> Option<String> {
    self.sighted().then(|| format!("sight: {} bots of another shard seen", self.strangers))
  }

  /// The line of the report on the bots that stopped before their first snapshot, and where each
  /// one stood, when some did.
  fn unjoined(&self) -> Option<String> {
    let total: usize = self.unjoined.values().sum();
    let phases: Vec<String> =
      self.unjoined.iter().map(|(phase, n)| format!("{phase} {n}")).collect();
    (total > 0).then(|| {
      format!("unjoined: {total} bots stopped before their first snapshot: {}", phases.join(", "))
    })
  }

  /// The line of the budget on that sight: a shard is a world of its own, so none is seen.
  fn sight_budget(&self) -> Option<(&'static str, bool)> {
    self.sighted().then_some(("no bot saw a bot of another shard", self.strangers == 0))
  }
}

impl Report {
  fn stat_lines(&self) -> [String; 5] {
    [
      stats("place in the queue at login", &self.position, "place"),
      stats("wait from login to session", &self.waited, "s"),
      stats("wait from login to the first snapshot", &self.entered, "s"),
      stats("data down for one client", &self.down, "B/s"),
      stats("data up for one client", &self.up, "B/s"),
    ]
  }
}

impl fmt::Display for Report {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let queued = self.queue.waiting;
    let mut lines = vec![format!(
      "load test: {} bots, {} played, {queued} waiting, {} failed",
      self.bots,
      self.played,
      self.bots.saturating_sub(self.played + queued)
    )];
    lines.extend(self.faults.iter().map(|(fault, bots)| format!("  {bots} bots: {fault}")));
    lines.extend(self.stat_lines());
    let Ticks { total, p50, p99, max, slow, late } = &self.ticks;
    lines.push(format!(
      "snapshots {}, refused by the world {}, largest {} bytes",
      self.snapshots, self.gaps, self.largest
    ));
    lines.push(self.queue.line());
    let bound = self.recovery.bound;
    if !bound.is_zero() {
      lines.push(self.recovery.line());
    }
    lines.extend(self.crossing.line());
    lines.extend(self.sight().into_iter().chain(self.unjoined()));
    lines.extend(self.drain.line());
    lines.push(format!(
      "ticks of the writers in {:.0} s: {total}, p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms, {slow} over 25 ms, {late} over 50 ms",
      self.window.as_secs_f64(),
      millis(*p50),
      millis(*p99),
      millis(*max),
    ));
    lines.extend(self.layers.iter().map(|(id, shard)| self.shard_line(*id, shard)));
    let capped =
      self.layers.values().all(|shard| shard.layers.values().all(|l| l.most <= shard.cap));
    let opened = self.layers.iter().all(|(id, shard)| {
      shard.layers.len() >= needed(self.stayed.get(id).copied().unwrap_or_default(), shard.cap)
    });
    let budget = [
      ("tick p99 at most 25 ms", *total > 0 && *p99 <= BUDGET),
      ("no tick of 50 ms or more", *total > 0 && *late == 0),
      ("data down, mean at most 5000 B/s", self.down.mean <= DOWN_MEAN),
      ("data down, p99 at most 20000 B/s", self.down.p99 <= DOWN_WORST),
      ("data up, mean at most 2000 B/s", self.up.mean <= UP_MEAN),
      ("no datagram over 1100 bytes", self.largest <= DATAGRAM),
      ("each bot played or waits in the queue", self.played + queued == self.bots),
      ("no layer over its cap", capped),
      ("each shard opened the layers that its players need", opened),
    ];
    let line = |(label, holds): (&str, bool)| format!("budget {label}: {}", verdict(holds));
    lines.extend(budget.map(line));
    lines.extend(self.queue.budget().map(line));
    if !bound.is_zero() {
      lines.extend(self.recovery.budget().map(|(label, holds)| line((&label, holds))));
    }
    lines.extend(self.crossing.budget().into_iter().map(line));
    lines.extend(self.sight_budget().map(line));
    lines.extend(self.drain.budget().into_iter().flatten().map(line));
    writeln!(f, "{}", lines.join("\n"))
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;
  use std::time::Duration;

  use mmo_client::Place;
  use mmo_protocol::{Layer, ZoneId};
  use tokio::time::Instant;

  use super::{Queue, Report, Stats, Ticks};
  use crate::bot::{End, Outcome, Role};
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
    let layers =
      layers.iter().map(|&(number, now, most)| (number, Players { now: Some(now), most }));
    Shard { cap, layers: layers.collect() }
  }

  /// A bot that entered the queue at the places `(ms, position)`, in a queue of `length`, and that
  /// waited when it ended at `end` ms, all from `base`.
  fn waiter(base: Instant, places: &[(u64, u32)], length: u32, end: u64) -> Outcome {
    let at = |ms| base + Duration::from_millis(ms);
    let places = places.iter().map(|&(ms, position)| (at(ms), Place { position, length, wait: 0 }));
    Outcome { places: places.collect(), queued: true, ended: Some(at(end)), ..Outcome::default() }
  }

  /// A bot that entered the queue at `enter` ms from `base`, and got its session.
  fn admitted(base: Instant, enter: u64) -> Outcome {
    let place = Place { position: 0, length: 1, wait: 0 };
    Outcome {
      places: vec![(base + Duration::from_millis(enter), place)],
      waited: Some(Duration::from_secs(1)),
      joined: Some(base),
      ..Outcome::default()
    }
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
    let report =
      Report::new(3, 0, Duration::ZERO, &outcomes, &counts, seen, Duration::from_secs(30));
    let lines = [
      "load test: 3 bots, 2 played, 0 waiting, 1 failed",
      "  1 bots: the link fails",
      "place in the queue at login (place, 2 bots): mean 2.0, p50 2.0, p99 2.0, max 2.0",
      "wait from login to session (s, 2 bots): mean 2.0, p50 1.0, p99 3.0, max 3.0",
      "wait from login to the first snapshot (s, 2 bots): mean 3.0, p50 2.0, p99 4.0, max 4.0",
      "data down for one client (B/s, 2 bots): mean 5000.0, p50 4000.0, p99 6000.0, max 6000.0",
      "data up for one client (B/s, 2 bots): mean 1500.0, p50 1000.0, p99 2000.0, max 2000.0",
      "snapshots 200, refused by the world 2, largest 900 bytes",
      "queue: 0 bots wait of 0 that must, the longest time without a place 0.0 s",
      "ticks of the writers in 30 s: 100, p50 2.75 ms, p99 15.25 ms, max 30.25 ms, 1 over 25 ms, 0 over 50 ms",
      "shard 1: 2 layers of cap 1 opened, 2 bots played, players on each layer: 1 1",
      "budget tick p99 at most 25 ms: ok",
      "budget no tick of 50 ms or more: ok",
      "budget data down, mean at most 5000 B/s: ok",
      "budget data down, p99 at most 20000 B/s: ok",
      "budget data up, mean at most 2000 B/s: ok",
      "budget no datagram over 1100 bytes: ok",
      "budget each bot played or waits in the queue: over",
      "budget no layer over its cap: over",
      "budget each shard opened the layers that its players need: ok",
      "budget the queue holds the bots that must wait: ok",
      "budget the queue admits in the order of entry: ok",
      "budget each waiting bot holds the place of its order of entry: ok",
      "budget each waiting bot gets its place at least each 7 s: ok",
    ];
    assert_eq!(report.to_string(), lines.join("\n") + "\n");
    let empty =
      Report::new(0, 0, Duration::ZERO, &[], &BTreeMap::new(), Seen::new(), Duration::from_secs(1));
    let text = empty.to_string();
    assert!(text.starts_with("load test: 0 bots, 0 played, 0 waiting, 0 failed\n"), "{text}");
    assert!(
      text.contains("budget tick p99 at most 25 ms: over\nbudget no tick of 50 ms or more: over\n")
    );
  }

  #[test]
  fn the_report_counts_the_crossers_that_entered_the_next_zone() {
    let layer = |zone, number| Layer { zone: ZoneId(zone), number };
    let one = |crossings| Outcome { role: Role::Crosser, crossings, ..played(1, 2, 0, 0, 0) };
    let crossed = one(vec![(layer(1, 3), layer(2, 1))]);
    let moved = one(vec![(layer(1, 3), layer(1, 4))]);
    let report = |outcomes: &[Outcome]| {
      let (zero, seen) = (Duration::ZERO, Seen::new());
      Report::new(outcomes.len(), 0, zero, outcomes, &BTreeMap::new(), seen, zero).to_string()
    };
    let walker = Outcome { crossings: vec![(layer(1, 3), layer(2, 5))], ..played(1, 2, 0, 0, 0) };
    let text = report(&[crossed, moved, one(vec![]), walker]);
    let mixed = "crossings: 3 moves to another layer, 1 of 3 crossers entered the next zone, 0 of 1 entries kept the number of their layer\n";
    assert!(text.contains(mixed), "{text}");
    let budget = "budget each crosser entered the next zone: over\nbudget each entry into the next zone kept the number of its layer: over\n";
    assert!(text.ends_with(budget), "{text}");
    let text = report(&[one(vec![(layer(1, 1), layer(2, 1))])]);
    let all = "crossings: 1 moves to another layer, 1 of 1 crossers entered the next zone, 1 of 1 entries kept the number of their layer\n";
    assert!(text.contains(all), "{text}");
    assert!(text.ends_with("entered the next zone: ok\nbudget each entry into the next zone kept the number of its layer: ok\n"), "{text}");
    let text = report(&[played(1, 2, 0, 0, 0)]);
    assert!(!text.contains("crossings") && !text.contains("crosser"), "{text}");
  }

  #[test]
  fn the_report_says_where_the_bots_that_never_played_stood() {
    let stopped = |phase| Outcome { phase, ..Outcome::default() };
    let faulted = Outcome { fault: Some("a fault".to_owned()), ..stopped("login") };
    let queued = Outcome { queued: true, ..stopped("login") };
    let one = played(1, 2, 0, 0, 0);
    let outcomes =
      [stopped("attach"), stopped("snapshot"), stopped("attach"), faulted, queued, one];
    let (zero, seen) = (Duration::ZERO, Seen::new());
    let text = Report::new(6, 0, zero, &outcomes, &BTreeMap::new(), seen, zero).to_string();
    let line =
      "unjoined: 4 bots stopped before their first snapshot: attach 2, login 1, snapshot 1\n";
    assert!(text.contains(line), "{text}");
    let (zero, seen) = (Duration::ZERO, Seen::new());
    let text =
      Report::new(1, 0, zero, &[played(1, 2, 0, 0, 0)], &BTreeMap::new(), seen, zero).to_string();
    assert!(!text.contains("unjoined"), "{text}");
  }

  #[test]
  fn the_report_counts_the_bots_of_another_shard_that_a_bot_saw_when_two_shards_play() {
    let on = |shard, seen: &[i64]| Outcome {
      shard,
      strangers: seen.iter().copied().collect(),
      ..played(1, 2, 0, 0, 0)
    };
    let report = |outcomes: &[Outcome]| {
      let (zero, seen) = (Duration::ZERO, Seen::new());
      Report::new(outcomes.len(), 0, zero, outcomes, &BTreeMap::new(), seen, zero).to_string()
    };
    let text = report(&[on(1, &[]), on(2, &[])]);
    assert!(text.contains("sight: 0 bots of another shard seen\n"), "{text}");
    assert!(text.ends_with("budget no bot saw a bot of another shard: ok\n"), "{text}");
    let text = report(&[on(1, &[7, 8]), on(2, &[9])]);
    assert!(text.contains("sight: 3 bots of another shard seen\n"), "{text}");
    assert!(text.ends_with("budget no bot saw a bot of another shard: over\n"), "{text}");
    let text = report(&[on(1, &[7]), on(1, &[])]);
    assert!(!text.contains("sight") && !text.contains("another shard"), "{text}");
  }

  #[test]
  fn a_shard_needs_layers_for_the_bots_that_stayed_in_its_zone_alone() {
    let layer = |zone, number| Layer { zone: ZoneId(zone), number };
    let gone = Outcome {
      role: Role::Crosser,
      crossings: vec![(layer(1, 2), layer(2, 1))],
      ..played(1, 2, 0, 0, 0)
    };
    let outcomes = [played(1, 2, 0, 0, 0), played(1, 2, 0, 0, 0), gone];
    let text = |layers| {
      let seen = Seen::from([(1, shard(2, layers))]);
      let zero = Duration::ZERO;
      Report::new(3, 0, zero, &outcomes, &BTreeMap::new(), seen, zero).to_string()
    };
    let opened = "budget each shard opened the layers that its players need: ";
    assert!(text(&[(1, 2, 2)]).contains(&format!("{opened}ok")));
    assert!(text(&[]).contains(&format!("{opened}over")));
  }

  #[test]
  fn the_queue_of_waiting_bots_is_ordered_placed_and_served_each_few_seconds() {
    let base = Instant::now();
    let none = Queue { waiting: 0, expected: 0, ordered: true, placed: true, gap: Duration::ZERO };
    assert_eq!(Queue::new(0, &[admitted(base, 0), failed("no")]), none);
    let outcomes = [
      waiter(base, &[(300, 2), (5300, 1)], 2, 10_000),
      admitted(base, 100),
      waiter(base, &[(200, 1), (5200, 0)], 2, 10_000),
      admitted(base, 0),
    ];
    let good = Queue::new(2, &outcomes);
    let ok =
      Queue { waiting: 2, expected: 2, ordered: true, placed: true, gap: Duration::from_secs(5) };
    assert_eq!(good, ok);
    let late = [waiter(base, &[(100, 0)], 1, 5000), admitted(base, 200)];
    let lagging = Queue {
      waiting: 1,
      expected: 1,
      ordered: false,
      placed: true,
      gap: Duration::from_millis(4900),
    };
    assert_eq!(Queue::new(1, &late), lagging);
    let far = [waiter(base, &[(0, 3)], 1, 5000)];
    assert!(!Queue::new(1, &far).placed);
    let short = [waiter(base, &[(0, 0)], 2, 5000)];
    assert!(!Queue::new(1, &short).placed);
    let slow = [waiter(base, &[(0, 0)], 1, 9000)];
    assert_eq!(Queue::new(1, &slow).gap, Duration::from_secs(9));
  }

  #[test]
  fn the_report_says_which_rule_of_the_queue_a_run_breaks() {
    let base = Instant::now();
    let outcomes =
      [admitted(base, 0), waiter(base, &[(100, 0)], 1, 9000), waiter(base, &[(200, 5)], 2, 9000)];
    let text =
      Report::new(3, 1, Duration::ZERO, &outcomes, &BTreeMap::new(), Seen::new(), Duration::ZERO)
        .to_string();
    assert!(text.starts_with("load test: 3 bots, 1 played, 2 waiting, 0 failed\n"), "{text}");
    let lines = [
      "queue: 2 bots wait of 1 that must, the longest time without a place 8.9 s",
      "budget each bot played or waits in the queue: ok",
      "budget the queue holds the bots that must wait: over",
      "budget the queue admits in the order of entry: ok",
      "budget each waiting bot holds the place of its order of entry: over",
      "budget each waiting bot gets its place at least each 7 s: over",
    ];
    for line in lines {
      assert!(text.contains(line), "{text} lacks {line}");
    }
  }

  #[test]
  fn the_report_says_how_long_the_bots_that_lost_their_link_took_to_play_again() {
    let mut back = played(1, 1, 1, 1, 1);
    back.recoveries = vec![Duration::from_secs(3), Duration::from_millis(4200)];
    let mut away = played(1, 1, 1, 1, 1);
    away.away = true;
    away.relocated = 1;
    let outcomes = [back, away, played(1, 1, 1, 1, 1)];
    let text = |bound| {
      Report::new(3, 0, bound, &outcomes, &BTreeMap::new(), Seen::new(), Duration::ZERO).to_string()
    };
    let none = text(Duration::ZERO);
    assert!(!none.contains("recovery"), "{none}");
    let report = text(Duration::from_secs(4));
    let lines = [
      "recovery: 2 links came back, the longest time without a snapshot 4.2 s, 1 bots still away, 1 on another layer",
      "budget each bot that lost its link played again: over",
      "budget each bot played again within 4 s: over",
      "budget each bot played again on its layer: over",
    ];
    for line in lines {
      assert!(report.contains(line), "{report} lacks {line}");
    }
    let mut slow = played(1, 1, 1, 1, 1);
    slow.recoveries = vec![Duration::from_secs(4)];
    let fine = [played(1, 1, 1, 1, 1), slow];
    let ok = Report::new(
      2,
      0,
      Duration::from_secs(4),
      &fine,
      &BTreeMap::new(),
      Seen::new(),
      Duration::ZERO,
    )
    .to_string();
    assert!(ok.contains("budget each bot played again within 4 s: ok"), "{ok}");
  }

  #[test]
  fn the_report_fails_a_drain_whose_bots_left_stop_or_whose_layers_stay_empty_or_sparse() {
    let stays = |end| Outcome { end, ..played(1, 2, 1, 1, 1) };
    let leaves = |end| Outcome { role: Role::Leaver, end, ..played(1, 2, 1, 1, 1) };
    let text = |outcomes: &[Outcome], mut shard: Shard| {
      shard.layers.insert(9, Players { now: None, most: 20 });
      let seen = Seen::from([(1, shard)]);
      let zero = Duration::ZERO;
      Report::new(outcomes.len(), 0, zero, outcomes, &BTreeMap::new(), seen, zero).to_string()
    };
    let good =
      text(&[leaves(End::LoggedOut), stays(End::Playing)], shard(20, &[(1, 17, 20), (3, 5, 13)]));
    let lines = [
      "drain: 1 of 1 bots logged out, 1 of 1 bots left play, 0 layers stand empty, 0 sparse layers fit in the others",
      "budget each bot left plays: ok",
      "budget no layer stands empty: ok",
      "budget no layer of a quarter of its cap or fewer fits in the other layers: ok",
    ];
    assert!(good.contains("players on each layer: 17 5 closed\n"), "{good}");
    for line in lines {
      assert!(good.contains(line), "{good} lacks {line}");
    }
    let bad = text(
      &[leaves(End::Playing), stays(End::Unseen)],
      shard(20, &[(1, 16, 16), (2, 0, 3), (3, 4, 4)]),
    );
    let lines = [
      "drain: 0 of 1 bots logged out, 0 of 1 bots left play, 1 layers stand empty, 1 sparse layers fit in the others",
      "budget each bot left plays: over",
      "budget no layer stands empty: over",
      "budget no layer of a quarter of its cap or fewer fits in the other layers: over",
    ];
    for line in lines {
      assert!(bad.contains(line), "{bad} lacks {line}");
    }
    let gone = text(&[leaves(End::LoggedOut), stays(End::LoggedOut)], shard(20, &[(1, 17, 20)]));
    assert!(gone.contains("budget each bot left plays: over"), "{gone}");
    let none = text(&[stays(End::Playing)], shard(20, &[(1, 1, 1)]));
    assert!(!none.contains("drain") && !none.contains("bot left"), "{none}");
  }

  #[test]
  fn the_report_fails_a_layer_over_its_cap_and_a_shard_that_opens_too_few_layers() {
    let outcomes = [played(1, 2, 1, 1, 1), played(1, 2, 1, 1, 1)];
    let text = |shard| {
      let seen = Seen::from([(1, shard)]);
      Report::new(2, 0, Duration::ZERO, &outcomes, &BTreeMap::new(), seen, Duration::from_secs(1))
        .to_string()
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
