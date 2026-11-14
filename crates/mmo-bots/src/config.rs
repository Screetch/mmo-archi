use std::num::ParseIntError;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mmo_protocol::ZoneId;

use crate::seed::letters;

/// What the load test reads from its environment, as the doc of the crate says.
#[derive(Clone)]
pub(crate) struct Config {
  pub(crate) authority: PathBuf,
  /// The address of the login service of each realm, in the order of the realms, with commas
  /// between them.
  pub(crate) login: String,
  pub(crate) gateway: String,
  pub(crate) database: String,
  pub(crate) redis: String,
  pub(crate) zone: ZoneId,
  pub(crate) bots: u32,
  /// The bots that the queue must hold when the measure ends.
  pub(crate) waiting: u32,
  /// The first bots, which start near the east border of the zone and walk east, which cross it.
  pub(crate) crossers: u32,
  /// The first bots, which log out once each bot plays, so the layers that they leave drain.
  pub(crate) leavers: u32,
  /// The count of the bots of each realm, in the order of the realms, or empty when the bots go
  /// round the realms.
  pub(crate) split: Vec<u32>,
  /// The last bots of the first realm, which log in together once each other bot plays, so they
  /// wait in its queue when it is full.
  pub(crate) rush: u32,
  pub(crate) town: u32,
  pub(crate) ramp: u32,
  pub(crate) join: Duration,
  pub(crate) play: Duration,
  /// The longest time without a snapshot that a bot that lost its link may take to play again. A
  /// bot attaches again with its token only when it is not zero.
  pub(crate) recover: Duration,
  pub(crate) run: String,
  pub(crate) seed: u64,
}

/// The most bots that one test may run, and the bound of `MMO_BOTS_COUNT`.
pub(crate) const MAX_BOTS: u32 = 500;

/// A variable of the environment that the load test cannot use.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("the variable {0} is not set")]
  Unset(&'static str),
  #[error("the variable {name} is not a number: {source}")]
  Number { name: &'static str, source: ParseIntError },
  #[error("the variable MMO_BOTS_COUNT holds {0} bots, over the bound of {max}", max = MAX_BOTS)]
  Count(u32),
  #[error(
    "the variable MMO_BOTS_CROSSERS holds {crossers} bots, over the {bots} of MMO_BOTS_COUNT"
  )]
  Crossers { crossers: u32, bots: u32 },
  #[error("the variable MMO_BOTS_LEAVERS holds {leavers} bots, over the {bots} of MMO_BOTS_COUNT")]
  Leavers { leavers: u32, bots: u32 },
  #[error("MMO_BOTS_RUN holds 1 to 6 letters from a to z, and {0:?} does not")]
  Run(String),
  #[error(
    "the variable MMO_BOTS_REALMS gives {given} counts of bots for {realms} realms, which hold {sum} bots of the {bots} of MMO_BOTS_COUNT"
  )]
  Split { given: usize, realms: u32, sum: u64, bots: u32 },
  #[error(
    "the variable MMO_BOTS_RUSH holds {rush} bots, and the first realm needs more bots than that, since the bot before the rush asks for a new character"
  )]
  Rush { rush: u32 },
}

type Var<'a> = &'a dyn Fn(&str) -> Option<String>;

fn text(var: Var, name: &'static str) -> Result<String, Error> {
  var(name).ok_or(Error::Unset(name))
}

/// The number in `name`, or `default` when the variable is not set and there is one.
fn number<T: FromStr<Err = ParseIntError>>(
  var: Var,
  name: &'static str,
  default: Option<T>,
) -> Result<T, Error> {
  match (var(name), default) {
    (Some(text), _) => text.parse().map_err(|source| Error::Number { name, source }),
    (None, Some(default)) => Ok(default),
    (None, None) => Err(Error::Unset(name)),
  }
}

impl Config {
  /// The count of realms that the bots play in, one for each login service.
  pub(crate) fn realms(&self) -> u32 {
    u32::try_from(self.login.split(',').count()).unwrap_or(u32::MAX)
  }

  /// The realm of the bot `index` and the address of its login service: the bots go round the
  /// realms, or fill each realm in its order with its count of `split`, and the first login service
  /// is the one of realm 1.
  pub(crate) fn realm(&self, index: u32) -> (u32, &str) {
    let (realm, _, _) = self.placed(index);
    (realm, self.login.split(',').nth(realm as usize - 1).unwrap_or_default())
  }

  /// The realm of the bot `index`, its rank among the bots of that realm, and the count of them.
  pub(crate) fn placed(&self, index: u32) -> (u32, u32, u32) {
    let mut before = 0;
    for (at, &count) in (1..).zip(&self.split) {
      if index < before + count {
        return (at, index - before, count);
      }
      before += count;
    }
    let realms = self.realms();
    let realm = index % realms;
    (realm + 1, index / realms, self.bots / realms + u32::from(realm < self.bots % realms))
  }

  /// The count of the bots of the realm `realm`.
  fn count(&self, realm: u32) -> u32 {
    (0..self.bots).filter(|&index| self.placed(index).0 == realm).count().try_into().unwrap_or(0)
  }

  /// Reads the config from `var`, which gives the value of a variable of the environment.
  pub(crate) fn read(var: Var) -> Result<Self, Error> {
    let seconds = |name, default| number(var, name, Some(default)).map(Duration::from_secs);
    let clock = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let run = var("MMO_BOTS_RUN").unwrap_or_else(|| letters(clock.as_secs(), 6));
    if !(1..=6).contains(&run.len()) || !run.bytes().all(|byte| byte.is_ascii_lowercase()) {
      return Err(Error::Run(run));
    }
    let config = Self {
      authority: text(var, "MMO_BOTS_AUTHORITY")?.into(),
      login: text(var, "MMO_BOTS_LOGIN")?,
      gateway: text(var, "MMO_BOTS_GATEWAY")?,
      database: text(var, "DATABASE_URL")?,
      redis: text(var, "REDIS_URL")?,
      zone: ZoneId(number(var, "MMO_BOTS_ZONE", None)?),
      bots: number(var, "MMO_BOTS_COUNT", None)?,
      waiting: number(var, "MMO_BOTS_WAITING", Some(0))?,
      crossers: number(var, "MMO_BOTS_CROSSERS", Some(0))?,
      leavers: number(var, "MMO_BOTS_LEAVERS", Some(0))?,
      split: var("MMO_BOTS_REALMS")
        .map(|text| {
          text
            .split(',')
            .map(|one| {
              one.parse().map_err(|source| Error::Number { name: "MMO_BOTS_REALMS", source })
            })
            .collect()
        })
        .transpose()?
        .unwrap_or_default(),
      rush: number(var, "MMO_BOTS_RUSH", Some(0))?,
      town: number(var, "MMO_BOTS_TOWN", Some(0))?,
      ramp: number(var, "MMO_BOTS_RAMP", Some(50))?,
      join: seconds("MMO_BOTS_JOIN_SECONDS", 60)?,
      play: seconds("MMO_BOTS_PLAY_SECONDS", 30)?,
      recover: seconds("MMO_BOTS_RECOVER_SECONDS", 0)?,
      run,
      seed: number(var, "MMO_BOTS_SEED", Some(1))?,
    };
    if config.bots > MAX_BOTS {
      return Err(Error::Count(config.bots));
    }
    if config.crossers > config.bots {
      return Err(Error::Crossers { crossers: config.crossers, bots: config.bots });
    }
    if config.leavers > config.bots {
      return Err(Error::Leavers { leavers: config.leavers, bots: config.bots });
    }
    let sum: u64 = config.split.iter().copied().map(u64::from).sum();
    if !config.split.is_empty()
      && (config.split.len() != config.realms() as usize || sum != u64::from(config.bots))
    {
      let (given, realms, bots) = (config.split.len(), config.realms(), config.bots);
      return Err(Error::Split { given, realms, sum, bots });
    }
    if config.rush > 0 && config.rush >= config.count(1) {
      return Err(Error::Rush { rush: config.rush });
    }
    Ok(config)
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;
  use std::time::Duration;

  use super::Config;

  const REQUIRED: [(&str, &str); 7] = [
    ("MMO_BOTS_AUTHORITY", "ca.pem"),
    ("MMO_BOTS_LOGIN", "localhost:4434"),
    ("MMO_BOTS_GATEWAY", "localhost:4436"),
    ("DATABASE_URL", "postgres://x@localhost/mmo"),
    ("REDIS_URL", "redis://localhost"),
    ("MMO_BOTS_ZONE", "1"),
    ("MMO_BOTS_COUNT", "500"),
  ];

  fn read(extra: &[(&'static str, &str)], without: &str) -> Result<Config, String> {
    let vars: BTreeMap<&str, String> = REQUIRED
      .iter()
      .chain(extra)
      .filter(|(name, _)| *name != without)
      .map(|(name, value)| (*name, (*value).to_owned()))
      .collect();
    Config::read(&|name| vars.get(name).cloned()).map_err(|error| error.to_string())
  }

  #[test]
  fn the_load_test_reads_its_variables_and_fills_the_optional_ones() {
    let config = read(&[], "").expect("a config");
    assert_eq!(
      (config.zone.0, config.bots, config.town, config.ramp, config.seed, config.run.len()),
      (1, 500, 0, 50, 1, 6)
    );
    assert_eq!((config.waiting, config.recover, config.crossers), (0, Duration::ZERO, 0));
    assert_eq!((config.leavers, config.split.len(), config.rush), (0, 0, 0));
    assert_eq!((config.join, config.play), (Duration::from_secs(60), Duration::from_secs(30)));
    let all = [
      ("MMO_BOTS_TOWN", "250"),
      ("MMO_BOTS_WAITING", "100"),
      ("MMO_BOTS_CROSSERS", "40"),
      ("MMO_BOTS_LEAVERS", "150"),
      ("MMO_BOTS_RECOVER_SECONDS", "10"),
      ("MMO_BOTS_RAMP", "10"),
      ("MMO_BOTS_JOIN_SECONDS", "5"),
      ("MMO_BOTS_PLAY_SECONDS", "6"),
      ("MMO_BOTS_RUN", "abc"),
      ("MMO_BOTS_SEED", "9"),
    ];
    let config = read(&all, "").expect("a config");
    assert_eq!((config.town, config.ramp, config.seed, config.run.as_str()), (250, 10, 9, "abc"));
    assert_eq!((config.waiting, config.recover), (100, Duration::from_secs(10)));
    assert_eq!((config.crossers, config.leavers), (40, 150));
    assert_eq!((config.join, config.play), (Duration::from_secs(5), Duration::from_secs(6)));
  }

  #[test]
  fn the_load_test_takes_the_most_bots_that_a_test_may_run() {
    let config = read(&[("MMO_BOTS_COUNT", "500")], "").expect("a config");
    assert_eq!(config.bots, 500);
  }

  #[test]
  fn the_bots_go_round_the_realms_that_the_login_services_name() {
    let one = read(&[], "").expect("a config");
    let first = (1, "localhost:4434");
    assert_eq!((one.realms(), one.realm(0), one.realm(7)), (1, first, first));
    let two =
      read(&[("MMO_BOTS_LOGIN", "a:1,b:2"), ("MMO_BOTS_COUNT", "5")], "").expect("a config");
    let at = [0, 1, 2].map(|index| two.realm(index));
    assert_eq!((two.realms(), at), (2, [(1, "a:1"), (2, "b:2"), (1, "a:1")]));
    let ranks = [0, 1, 2, 3, 4].map(|index| two.placed(index));
    assert_eq!(ranks, [(1, 0, 3), (2, 0, 2), (1, 1, 3), (2, 1, 2), (1, 2, 3)]);
  }

  #[test]
  fn the_bots_fill_each_realm_with_its_count_and_the_rush_takes_the_last_of_the_first() {
    let split = [
      ("MMO_BOTS_LOGIN", "a:1,b:2,c:3"),
      ("MMO_BOTS_COUNT", "6"),
      ("MMO_BOTS_REALMS", "3,1,2"),
      ("MMO_BOTS_RUSH", "2"),
    ];
    let three = read(&split, "").expect("a config");
    let ranks = [0, 1, 2, 3, 4, 5].map(|index| three.placed(index));
    assert_eq!(ranks, [(1, 0, 3), (1, 1, 3), (1, 2, 3), (2, 0, 1), (3, 0, 2), (3, 1, 2)]);
    assert_eq!([3, 4].map(|index| three.realm(index)), [(2, "b:2"), (3, "c:3")]);
    assert_eq!((three.split, three.rush), (vec![3, 1, 2], 2));
  }

  #[test]
  fn the_load_test_names_the_variable_that_it_cannot_use() {
    let unset: Vec<_> =
      REQUIRED.iter().map(|(name, _)| read(&[], name).err().expect("a refusal")).collect();
    let expected: Vec<_> =
      REQUIRED.iter().map(|(name, _)| format!("the variable {name} is not set")).collect();
    assert_eq!(unset, expected);
    let number = read(&[("MMO_BOTS_COUNT", "many")], "").err();
    assert_eq!(
      number.as_deref(),
      Some("the variable MMO_BOTS_COUNT is not a number: invalid digit found in string")
    );
    for name in [
      "MMO_BOTS_WAITING",
      "MMO_BOTS_CROSSERS",
      "MMO_BOTS_LEAVERS",
      "MMO_BOTS_RUSH",
      "MMO_BOTS_RECOVER_SECONDS",
      "MMO_BOTS_TOWN",
      "MMO_BOTS_RAMP",
      "MMO_BOTS_JOIN_SECONDS",
      "MMO_BOTS_PLAY_SECONDS",
      "MMO_BOTS_SEED",
    ] {
      let bad = read(&[(name, "x")], "").err();
      assert_eq!(
        bad,
        Some(format!("the variable {name} is not a number: invalid digit found in string"))
      );
    }
    let over = read(&[("MMO_BOTS_COUNT", "501")], "").err();
    assert_eq!(
      over.as_deref(),
      Some("the variable MMO_BOTS_COUNT holds 501 bots, over the bound of 500")
    );
    let many = read(&[("MMO_BOTS_COUNT", "10"), ("MMO_BOTS_CROSSERS", "11")], "").err();
    assert_eq!(
      many.as_deref(),
      Some("the variable MMO_BOTS_CROSSERS holds 11 bots, over the 10 of MMO_BOTS_COUNT")
    );
    let many = read(&[("MMO_BOTS_COUNT", "10"), ("MMO_BOTS_LEAVERS", "11")], "").err();
    assert_eq!(
      many.as_deref(),
      Some("the variable MMO_BOTS_LEAVERS holds 11 bots, over the 10 of MMO_BOTS_COUNT")
    );
  }

  #[test]
  fn the_load_test_refuses_a_count_of_each_realm_and_a_rush_that_do_not_fit_its_bots() {
    let realms = [("MMO_BOTS_LOGIN", "a:1,b:2"), ("MMO_BOTS_COUNT", "5")];
    let mut refused = Vec::new();
    for (name, value) in [
      ("MMO_BOTS_REALMS", "5"),
      ("MMO_BOTS_REALMS", "2,2"),
      ("MMO_BOTS_REALMS", "2,x"),
      ("MMO_BOTS_RUSH", "3"),
    ] {
      refused.push(read(&[&realms[..], &[(name, value)]].concat(), "").err());
    }
    let split = "the variable MMO_BOTS_REALMS gives";
    assert_eq!(
      refused,
      [
        Some(format!("{split} 1 counts of bots for 2 realms, which hold 5 bots of the 5 of MMO_BOTS_COUNT")),
        Some(format!("{split} 2 counts of bots for 2 realms, which hold 4 bots of the 5 of MMO_BOTS_COUNT")),
        Some("the variable MMO_BOTS_REALMS is not a number: invalid digit found in string".to_owned()),
        Some("the variable MMO_BOTS_RUSH holds 3 bots, and the first realm needs more bots than that, since the bot before the rush asks for a new character".to_owned()),
      ]
    );
  }

  #[test]
  fn the_load_test_refuses_a_run_that_is_not_1_to_6_letters() {
    for run in ["", "Abc", "abcdefg", "a1"] {
      let bad = read(&[("MMO_BOTS_RUN", run)], "").err().expect("a refusal");
      assert_eq!(
        bad,
        format!("MMO_BOTS_RUN holds 1 to 6 letters from a to z, and {run:?} does not")
      );
    }
  }
}
