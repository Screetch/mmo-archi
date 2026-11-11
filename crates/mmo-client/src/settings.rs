//! The settings that a binary of the client reads from its environment, and why such a binary
//! fails.

use std::path::Path;
use std::time::Duration;

use mmo_protocol::{Password, ToClient};

use crate::{Account, Fault, Trust, WindowError};

/// Why a binary of the client does not reach the world, or does not show it.
#[derive(Debug, thiserror::Error)]
pub enum Failure {
  /// The environment lacks a variable.
  #[error("the variable {0} is not set")]
  Unset(String),
  /// The command line holds what the binary does not take.
  #[error("{0}")]
  Usage(String),
  /// The client cannot reach a server, or the login service refuses it.
  #[error(transparent)]
  Fault(#[from] Fault),
  /// The check did not end in time.
  #[error("the check does not end within {0:?}")]
  Slow(Duration),
  /// The gateway sends a message that is neither its zone nor a snapshot, so the player is not
  /// in the zone.
  #[error("the gateway sends {0:?} in place of a snapshot")]
  Message(ToClient),
  /// The window or the drawing fails.
  #[error(transparent)]
  Window(#[from] WindowError),
}

/// What a binary needs to log in and to link to a gateway.
pub(crate) struct Settings {
  pub(crate) trust: Trust,
  pub(crate) login: String,
  pub(crate) gateway: String,
  pub(crate) account: Account,
}

impl Settings {
  /// Reads the variables `<prefix>_AUTHORITY`, `_LOGIN`, `_GATEWAY`, `_NAME`, `_PASSWORD` and
  /// `_CHARACTER` of the environment that `read` gives. `AUTHORITY` is the PEM file of the
  /// certificates that sign the login service and the gateway, and `LOGIN` and `GATEWAY` are their
  /// addresses as `host:port`, where each host is the name that its certificate must hold.
  pub(crate) fn read(
    prefix: &str,
    read: &dyn Fn(&str) -> Option<String>,
    create: bool,
  ) -> Result<Self, Failure> {
    let text = |name: &str| {
      let name = format!("{prefix}_{name}");
      read(&name).ok_or(Failure::Unset(name))
    };
    let trust = Trust::from_pem(Path::new(&text("AUTHORITY")?))?;
    let (login, gateway) = (text("LOGIN")?, text("GATEWAY")?);
    let account = Account {
      name: text("NAME")?,
      password: Password(text("PASSWORD")?),
      character: text("CHARACTER")?,
      create,
    };
    Ok(Self { trust, login, gateway, account })
  }
}
