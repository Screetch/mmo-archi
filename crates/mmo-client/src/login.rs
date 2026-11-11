//! The login of the client: the queue of the world, the character, and the session.

use std::future::pending;
use std::time::Duration;

use mmo_protocol::{FromLogin, Password, ToLogin, Token};
use tokio::time::sleep;

use crate::Fault;
use crate::connect::{Trust, open};

/// The client says each 5 s that it still waits in the queue.
const WAIT: Duration = Duration::from_secs(5);

/// The account to sign in to, and the character to play.
#[derive(Clone, Debug)]
pub struct Account {
  /// The name of the account.
  pub name: String,
  /// The password of the account.
  pub password: Password,
  /// The name of the character to play. The login service writes a name with its first letter in
  /// upper case, and the client finds the character with no regard to the case.
  pub character: String,
  /// Makes the account first, as [`ToLogin::NewAccount`] does, in place of a login.
  pub create: bool,
}

/// The place of the account in the queue of the world, as [`FromLogin::Queued`] gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
  /// The place of the account: the first account of the queue is at 0.
  pub position: u32,
  /// The count of accounts in the queue.
  pub length: u32,
  /// The estimate of the wait, in seconds.
  pub wait: u32,
}

/// Signs in to the login service at `address`, as `host:port`, waits in the queue of the world,
/// makes the character when the account has none of that name, selects it, and gives the token of
/// the session for a gateway. It calls `queued` with the place of the account in the queue each
/// time that the login service gives it, which is each 5 s while the account waits.
pub async fn enter(
  trust: &Trust,
  address: &str,
  account: &Account,
  queued: &mut dyn FnMut(Place),
) -> Result<Token, Fault> {
  enter_with(trust, address, account, queued, WAIT).await
}

async fn enter_with(
  trust: &Trust,
  address: &str,
  account: &Account,
  queued: &mut dyn FnMut(Place),
  wait: Duration,
) -> Result<Token, Fault> {
  let mut link = open(trust, address).await?;
  let Account { name, password, create, .. } = account.clone();
  let mut outgoing = Some(if create {
    ToLogin::NewAccount { name, password }
  } else {
    ToLogin::Login { name, password }
  });
  let (mut waiting, mut made) = (false, false);
  loop {
    if let Some(message) = outgoing.take() {
      link.write(&message).await?;
    }
    let answer = tokio::select! {
      answer = link.read::<FromLogin>() => answer?,
      () = async { if waiting { sleep(wait).await } else { pending().await } } => {
        outgoing = Some(ToLogin::Wait);
        continue;
      }
    };
    match answer {
      FromLogin::Refused(why) => return Err(Fault::Refused(why)),
      FromLogin::Queued { position, length, wait, .. } => {
        waiting = true;
        queued(Place { position, length, wait });
      }
      FromLogin::Characters(characters) => {
        waiting = false;
        let found = characters.iter().find(|one| one.name.eq_ignore_ascii_case(&account.character));
        outgoing = Some(match found {
          Some(one) => ToLogin::Select { character: one.id },
          None if !made => ToLogin::NewCharacter { name: account.character.clone() },
          None => return Err(Fault::NoCharacter(account.character.clone())),
        });
        made |= found.is_none();
      }
      FromLogin::Session { token } => return Ok(token),
    }
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use mmo_protocol::{Character, CharacterId, FromLogin, Password, Refusal, ToLogin, Token};

  use mmo_protocol::frame;

  use super::{Account, Place, WAIT, enter, enter_with};
  use crate::testing::{Fake, hold};

  const TOKEN: Token = Token([9; 32]);

  fn account(create: bool) -> Account {
    Account {
      name: "ada".to_owned(),
      password: Password("a long password".to_owned()),
      character: "brom".to_owned(),
      create,
    }
  }

  fn character(name: &str) -> Character {
    Character { id: CharacterId(42), name: name.to_owned() }
  }

  async fn run(fake: &Fake, account: &Account) -> (Result<Token, String>, Vec<Place>) {
    let mut places = Vec::new();
    let wait = Duration::from_millis(20);
    let token =
      enter_with(&fake.trust, &fake.address, account, &mut |place| places.push(place), wait)
        .await
        .map_err(|fault| fault.to_string());
    (token, places)
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_client_waits_in_the_queue_then_selects_its_character() {
    let fake = Fake::start(|mut peer| async move {
      let login =
        ToLogin::Login { name: "ada".to_owned(), password: Password("a long password".to_owned()) };
      assert_eq!(peer.read::<ToLogin>().await, login);
      let queued = |position| FromLogin::Queued { position, length: 9, wait: 30, resume: TOKEN };
      peer.write(&queued(4)).await;
      assert_eq!(peer.read::<ToLogin>().await, ToLogin::Wait);
      peer.write(&queued(3)).await;
      peer.write(&FromLogin::Characters(vec![character("Ada"), character("Brom")])).await;
      assert_eq!(peer.read::<ToLogin>().await, ToLogin::Select { character: CharacterId(42) });
      peer.write(&FromLogin::Session { token: TOKEN }).await;
      peer
    });
    let (token, places) = run(&fake, &account(false)).await;
    assert_eq!(token, Ok(TOKEN));
    let place = |position| Place { position, length: 9, wait: 30 };
    assert_eq!(places, [place(4), place(3)]);
    fake.end().await;
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn a_new_account_makes_the_character_that_it_lacks() {
    let fake = Fake::start(|mut peer| async move {
      let new = ToLogin::NewAccount {
        name: "ada".to_owned(),
        password: Password("a long password".to_owned()),
      };
      assert_eq!(peer.read::<ToLogin>().await, new);
      peer.write(&FromLogin::Characters(vec![])).await;
      assert_eq!(peer.read::<ToLogin>().await, ToLogin::NewCharacter { name: "brom".to_owned() });
      peer.write(&FromLogin::Characters(vec![character("Brom")])).await;
      assert_eq!(peer.read::<ToLogin>().await, ToLogin::Select { character: CharacterId(42) });
      peer.write(&FromLogin::Session { token: TOKEN }).await;
      peer
    });
    assert_eq!(run(&fake, &account(true)).await, (Ok(TOKEN), vec![]));
    fake.end().await;
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_client_stops_on_a_refusal_and_on_a_character_that_the_login_does_not_give() {
    let refusing = Fake::start(|mut peer| async move {
      peer.read::<ToLogin>().await;
      peer.write(&FromLogin::Refused(Refusal::Password)).await;
      peer
    });
    let (token, _) = run(&refusing, &account(false)).await;
    assert_eq!(token, Err("the login service refuses: Password".to_owned()));
    refusing.end().await;
    let stubborn = Fake::start(|mut peer| async move {
      peer.read::<ToLogin>().await;
      peer.write(&FromLogin::Characters(vec![character("Ada")])).await;
      peer.read::<ToLogin>().await;
      peer.write(&FromLogin::Characters(vec![character("Ada")])).await;
      peer
    });
    let (token, _) = run(&stubborn, &account(false)).await;
    assert_eq!(token, Err("the account has no character named brom".to_owned()));
    stubborn.end().await;
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_login_of_the_game_waits_5_s_and_gives_the_session() {
    let fake = Fake::start(|mut peer| async move {
      peer.read::<ToLogin>().await;
      peer.write(&FromLogin::Queued { position: 0, length: 1, wait: 0, resume: TOKEN }).await;
      peer.write(&FromLogin::Characters(vec![character("Brom")])).await;
      peer.read::<ToLogin>().await;
      peer.write(&FromLogin::Session { token: TOKEN }).await;
      peer
    });
    let token = enter(&fake.trust, &fake.address, &account(false), &mut drop).await;
    assert_eq!(token.ok(), Some(TOKEN));
    fake.end().await;
  }

  #[expect(clippy::disallowed_methods, reason = "the test starts its runtime once")]
  #[tokio::test]
  async fn the_client_names_what_keeps_it_from_the_login() {
    let fake = Fake::start(hold);
    let mut long = account(false);
    long.name = "a".repeat(70_000);
    let (token, _) = run(&fake, &long).await;
    assert_eq!(token, Err("a frame holds 65536 bytes at most".to_owned()));
    let nowhere = enter_with(&fake.trust, "127.0.0.1:0", &account(false), &mut drop, WAIT).await;
    assert!(nowhere.is_err());
    fake.end().await;
    let garbling = Fake::start(|mut peer| async move {
      peer.read::<ToLogin>().await;
      peer.write_raw(&frame(&[0xff, 0xff, 0xff]).expect("a frame")).await;
      peer
    });
    let (token, _) = run(&garbling, &account(false)).await;
    assert_eq!(
      token,
      Err("the bytes are not a message of this version of the protocol".to_owned())
    );
    garbling.end().await;
  }
}
