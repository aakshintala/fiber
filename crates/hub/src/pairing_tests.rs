//! Pairing records, secrets, consumption and revocation under an injected clock.
use super::*;
use crate::fake::FakeStarter;
use crate::testkit::args;
use contract::clock::Clock;
use fakes::MUST_SUCCEED_WITHIN;
use fakes::clock::FakeClock;
use serde_json::json;
use std::fs;

struct Setup {
    _temp: fakes::TempDir,
    hub: Hub,
    clock: Arc<FakeClock>,
}
impl Setup {
    fn new() -> Self {
        let (temp, dir) = crate::testkit::home("pairing");
        let clock = FakeClock::new();
        let mut hub = crate::testkit::hub(
            &dir,
            Arc::new(FakeStarter::hang(&dir)),
            Arc::clone(&clock) as Arc<dyn Clock>,
        );
        let mut draw = 0u8;
        hub.pairing = Pairing::with_random(Box::new(move |bytes| {
            draw = draw.wrapping_add(1);
            bytes.fill(draw);
            Ok(())
        }));
        Self {
            _temp: temp,
            hub,
            clock,
        }
    }
    fn mint(&self, device: &str, who: &Who) -> String {
        match self
            .hub
            .pairing
            .mint(&self.hub, who, &args(json!({"device":device})))
            .unwrap()
        {
            Outcome::Done(value) => value["code"].as_str().unwrap().to_owned(),
            Outcome::Dropped => panic!("mint was dropped"),
        }
    }
    fn pair(&self, device: &str, n: u64) -> (DeviceToken, Who) {
        let code = self.mint(device, &Who::Local);
        self.hub
            .pairing
            .redeem(&self.hub, n, &args(json!({"code":code})))
            .ok()
            .unwrap()
    }
    fn revoke(&self, device: &str) -> Revoked {
        match self
            .hub
            .pairing
            .revoke(&self.hub, &Who::Local, &args(json!({"device":device})))
            .unwrap()
        {
            Outcome::Done(value) => value,
            Outcome::Dropped => panic!("revoke was dropped"),
        }
    }
    fn record(&self, device: &str) -> Value {
        serde_json::from_slice(&fs::read(self.hub.home.join("hub/devices").join(device)).unwrap())
            .unwrap()
    }
}

/// Runs `finish` where it owes no notice and requires it back promptly.
///
/// Under the `finish` mutants that widen its wait (`!=` to `==`, `&&` to
/// `||`) the empty wait runs out its whole deadline on the frozen fake
/// clock, so the wall-clock bound below fails the test in seconds instead
/// of the mutant timeout. Goes over the public API with the injected
/// clock, so no test hook is needed.
fn finish_now(hub: &Hub, clock: &FakeClock, revoked: Revoked) {
    let end = revoked.end;
    std::thread::scope(|s| {
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        s.spawn(move || {
            hub.pairing.finish(hub, revoked);
            finished_tx.send(()).unwrap_or(());
        });
        if fakes::Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&finished_rx)
            .is_ok()
        {
            return;
        }
        clock.advance_when_parked_threads(
            Some(end),
            2,
            crate::port::STOP_CLOSE_WAIT,
            &fakes::Deadline::after(MUST_SUCCEED_WITHIN),
        );
        fakes::Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&finished_rx, "finish exits at its deadline");
        panic!("finish waited for fake time with nothing to wait for");
    });
}

#[test]
fn finish_with_nothing_to_wait_for_returns_without_fake_time() {
    let setup = Setup::new();
    setup.pair("phone", 1);
    let revoked = setup.revoke("phone");
    // The fake clock is never advanced: finish must not wait on it.
    finish_now(&setup.hub, &setup.clock, revoked);
}

#[test]
fn pairing_code_for_a_paired_name_is_rejected() {
    let setup = Setup::new();
    setup.pair("phone", 1);
    let error = setup
        .hub
        .pairing
        .mint(&setup.hub, &Who::Local, &args(json!({"device":"phone"})))
        .err()
        .unwrap();
    assert_eq!(
        error,
        Refusal {
            code: ErrorCode::InvalidArguments,
            message: "Device phone is already paired; revoke it first.".to_owned(),
        }
    );
}

#[test]
fn a_second_code_for_a_name_replaces_the_first() {
    let mut setup = Setup::new();
    let mut draw = 0;
    setup.hub.pairing = Pairing::with_random(Box::new(move |bytes| {
        draw += 1;
        bytes.fill(if draw <= 2 { 1 } else { 2 });
        Ok(())
    }));
    let old = setup.mint("phone", &Who::Local);
    let new = setup.mint("phone", &Who::Local);
    assert_ne!(old, new);
    assert!(matches!(
        setup
            .hub
            .pairing
            .redeem(&setup.hub, 1, &args(json!({"code":old}))),
        Err(Refused::Pairing)
    ));
    assert!(
        setup
            .hub
            .pairing
            .redeem(&setup.hub, 1, &args(json!({"code":new})))
            .is_ok()
    );
}

#[test]
fn revoke_of_an_unpaired_device_is_not_found() {
    let setup = Setup::new();
    assert_eq!(
        setup
            .hub
            .pairing
            .revoke(&setup.hub, &Who::Local, &args(json!({"device":"nope"})))
            .err()
            .unwrap(),
        Refusal {
            code: ErrorCode::NotFound,
            message: "No device named nope is paired.".to_owned(),
        }
    );
}

#[test]
fn revoke_is_io_failed_when_the_device_directory_cannot_be_read() {
    let setup = Setup::new();
    fs::create_dir_all(setup.hub.home.join("hub")).unwrap();
    fs::write(setup.hub.home.join("hub/devices"), "obstacle").unwrap();
    let refused = setup
        .hub
        .pairing
        .revoke(&setup.hub, &Who::Local, &args(json!({"device":"phone"})))
        .err()
        .unwrap();
    assert_eq!(refused.code, ErrorCode::IoFailed);
}

#[test]
fn revoke_never_reaches_outside_hub_devices() {
    let setup = Setup::new();
    fs::create_dir_all(setup.hub.home.join("hub")).unwrap();
    fs::write(setup.hub.home.join("hub/x"), "protected").unwrap();
    assert_eq!(
        setup
            .hub
            .pairing
            .revoke(&setup.hub, &Who::Local, &args(json!({"device":"../x"})))
            .err()
            .unwrap()
            .code,
        ErrorCode::InvalidArguments
    );
    assert_eq!(
        fs::read(setup.hub.home.join("hub/x")).unwrap(),
        b"protected"
    );
}

#[test]
fn a_failed_record_write_is_io_failed_and_the_code_stays_used() {
    let setup = Setup::new();
    let code = setup.mint("phone", &Who::Local);
    fs::create_dir_all(setup.hub.home.join("hub")).unwrap();
    fs::write(setup.hub.home.join("hub/devices"), "obstacle").unwrap();
    assert!(matches!(
        setup
            .hub
            .pairing
            .redeem(&setup.hub, 1, &args(json!({"code":code}))),
        Err(Refused::Io(_))
    ));
    fs::remove_file(setup.hub.home.join("hub/devices")).unwrap();
    assert!(matches!(
        setup
            .hub
            .pairing
            .redeem(&setup.hub, 1, &args(json!({"code":code}))),
        Err(Refused::Pairing)
    ));
    assert!(!setup.hub.home.join("hub/.phone.tmp").exists());
}

#[test]
fn authenticate_updates_last_connected() {
    let setup = Setup::new();
    let (token, _) = setup.pair("phone", 1);
    let before = setup.record("phone");
    // No code under test waits on the clock here; no idle runner or clock waiter exists in this direct API fixture (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    setup.clock.advance(Duration::from_secs(5));
    assert!(
        setup
            .hub
            .pairing
            .authenticate(&setup.hub, 2, &token.0)
            .is_some()
    );
    let after = setup.record("phone");
    assert_eq!(after["paired_at"], before["paired_at"]);
    assert_eq!(
        after["last_connected_at"].as_u64(),
        Some(before["last_connected_at"].as_u64().unwrap() + 5000)
    );
}

#[test]
fn a_failed_last_connection_write_still_authenticates() {
    let setup = Setup::new();
    let (token, _) = setup.pair("phone", 1);
    let before = setup.record("phone");
    fs::create_dir(setup.hub.home.join("hub/.phone.tmp")).unwrap();
    assert!(
        setup
            .hub
            .pairing
            .authenticate(&setup.hub, 2, &token.0)
            .is_some()
    );
    assert_eq!(setup.record("phone"), before);
    let log = fs::read_to_string(setup.hub.home.join("logs/hub.log")).unwrap();
    assert!(log.contains("warn") && log.contains("io_failed") && log.contains("hub/devices/phone"));
}

#[test]
fn revoking_a_device_voids_the_codes_it_minted() {
    let setup = Setup::new();
    let (_, phone) = setup.pair("phone", 1);
    let code = setup.mint("tablet", &phone);
    let revoked = setup.revoke("phone");
    assert!(phone.revoked());
    assert_eq!(setup.hub.pairing.pending(), 0);
    assert!(matches!(
        setup
            .hub
            .pairing
            .redeem(&setup.hub, 2, &args(json!({"code":code}))),
        Err(Refused::Pairing)
    ));
    assert!(matches!(
        setup
            .hub
            .pairing
            .mint(&setup.hub, &phone, &args(json!({"device":"x"})))
            .unwrap(),
        Outcome::Dropped
    ));
    assert!(matches!(
        setup
            .hub
            .pairing
            .revoke(&setup.hub, &phone, &args(json!({"device":"x"})))
            .unwrap(),
        Outcome::Dropped
    ));
    finish_now(&setup.hub, &setup.clock, revoked);
}

#[test]
fn a_revoked_connection_attached_late_is_shut_at_once() {
    use std::io::Read as _;
    let setup = Setup::new();
    let (_, who) = setup.pair("phone", 1);
    let revoked = setup.revoke("phone");
    let (writer, mut peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
    setup
        .hub
        .pairing
        .attach(&who, &Arc::new(Mutex::new(writer)));
    assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);
    finish_now(&setup.hub, &setup.clock, revoked);
}

#[test]
fn a_hash_only_record_outside_the_name_rule_authenticates() {
    let setup = Setup::new();
    fs::create_dir_all(setup.hub.home.join("hub/devices")).unwrap();
    let path = setup.hub.home.join("hub/devices/.phone");
    let text = json!({"token_sha256":devices::sha256_hex(b"token")}).to_string();
    fs::write(&path, &text).unwrap();
    let who = setup
        .hub
        .pairing
        .authenticate(&setup.hub, 1, "token")
        .unwrap();
    assert_eq!(who.by(), ".phone");
    assert_eq!(fs::read_to_string(path).unwrap(), text);
    assert_eq!(
        setup
            .hub
            .pairing
            .devices(&setup.hub, &Map::new())
            .unwrap()
            .unwrap()["devices"][0]["name"],
        ".phone"
    );
    assert_eq!(
        setup
            .hub
            .pairing
            .revoke(&setup.hub, &Who::Local, &args(json!({"device":".phone"})))
            .err()
            .unwrap()
            .code,
        ErrorCode::InvalidArguments
    );
}

#[test]
fn a_sync_failure_after_rename_still_pairs() {
    let setup = Setup::new();
    let code = setup.mint("phone", &Who::Local);
    devices::fault(devices::Fault::Sync);
    let (token, who) = setup
        .hub
        .pairing
        .redeem(&setup.hub, 1, &args(json!({"code":code})))
        .ok()
        .unwrap();
    assert_eq!(
        devices::holder(&setup.hub.home, &token.0).as_deref(),
        Some("phone")
    );
    setup.hub.pairing.announce_owed(&setup.hub, &who);
    let log = fs::read_to_string(setup.hub.home.join("logs/hub.log")).unwrap();
    assert!(log.contains("device_paired") && log.contains("io_failed") && log.contains("warn"));
    assert!(!log.contains(&token.0) && !log.contains(&code));
}

#[test]
fn a_sync_failure_after_unlink_still_revokes() {
    let setup = Setup::new();
    let (_, who) = setup.pair("phone", 1);
    devices::fault(devices::Fault::Sync);
    let revoked = setup.revoke("phone");
    assert!(who.revoked());
    assert!(!setup.hub.home.join("hub/devices/phone").exists());
    finish_now(&setup.hub, &setup.clock, revoked);
    let log = fs::read_to_string(setup.hub.home.join("logs/hub.log")).unwrap();
    assert!(log.contains("device_revoked") && log.contains("io_failed") && log.contains("warn"));
}

#[test]
fn a_failed_unlink_changes_nothing() {
    let setup = Setup::new();
    let (_, who) = setup.pair("phone", 1);
    setup.mint("tablet", &who);
    devices::fault(devices::Fault::Unlink);
    assert_eq!(
        setup
            .hub
            .pairing
            .revoke(&setup.hub, &Who::Local, &args(json!({"device":"phone"})))
            .err()
            .unwrap()
            .code,
        ErrorCode::IoFailed
    );
    assert!(!who.revoked());
    assert_eq!(setup.hub.pairing.pending(), 1);
    assert!(setup.hub.home.join("hub/devices/phone").exists());
}

#[test]
fn a_repeated_draw_never_overwrites_another_devices_code() {
    let mut setup = Setup::new();
    let mut draw = 0;
    setup.hub.pairing = Pairing::with_random(Box::new(move |bytes| {
        draw += 1;
        bytes.fill(if draw <= 2 { 1 } else { 2 });
        Ok(())
    }));
    let a = setup.mint("a", &Who::Local);
    let b = setup.mint("b", &Who::Local);
    assert_ne!(a, b);
    assert_eq!(
        setup
            .hub
            .pairing
            .redeem(&setup.hub, 1, &args(json!({"code":a})))
            .ok()
            .unwrap()
            .1
            .by(),
        "a"
    );
    assert_eq!(
        setup
            .hub
            .pairing
            .redeem(&setup.hub, 2, &args(json!({"code":b})))
            .ok()
            .unwrap()
            .1
            .by(),
        "b"
    );
    let mut setup = Setup::new();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = Arc::clone(&count);
    setup.hub.pairing = Pairing::with_random(Box::new(move |bytes| {
        calls.fetch_add(1, Ordering::SeqCst);
        bytes.fill(0);
        Ok(())
    }));
    let old = setup.mint("a", &Who::Local);
    assert_eq!(
        setup
            .hub
            .pairing
            .mint(&setup.hub, &Who::Local, &args(json!({"device":"a"})))
            .err()
            .unwrap(),
        Refusal {
            code: ErrorCode::IoFailed,
            message: "The system has no random bytes.".to_owned(),
        }
    );
    assert_eq!(count.load(Ordering::SeqCst), 9);
    assert_eq!(setup.hub.pairing.pending(), 1);
    assert!(
        setup
            .hub
            .pairing
            .redeem(&setup.hub, 1, &args(json!({"code":old})))
            .is_ok()
    );
}

#[test]
fn a_token_copied_into_two_records_dies_with_either() {
    let setup = Setup::new();
    let (token, phone) = setup.pair("phone", 1);
    let path = setup.hub.home.join("hub/devices/phone");
    let tablet_path = setup.hub.home.join("hub/devices/tablet");
    fs::rename(&path, &tablet_path).unwrap();
    let tablet = setup
        .hub
        .pairing
        .authenticate(&setup.hub, 2, &token.0)
        .unwrap();
    fs::copy(&tablet_path, &path).unwrap();
    let (_, other) = setup.pair("other", 3);
    let revoked = setup.revoke("phone");
    assert!(phone.revoked() && tablet.revoked());
    assert!(!other.revoked());
    assert!(!path.exists() && !tablet_path.exists());
    assert!(devices::holder(&setup.hub.home, &token.0).is_none());
    assert!(setup.hub.home.join("hub/devices/other").exists());
    assert!(
        setup
            .hub
            .pairing
            .authenticate(&setup.hub, 4, &token.0)
            .is_none()
    );
    finish_now(&setup.hub, &setup.clock, revoked);
}

#[test]
fn authenticate_holds_revoke_until_it_registers() {
    let setup = Setup::new();
    let (token, _) = setup.pair("phone", 1);
    let clock = Arc::clone(&setup.clock);
    let hub = Arc::new(setup.hub);
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    hub.pairing.pause_in_lock(Pause::AfterFound, move || {
        held_tx.send(()).unwrap();
        release_rx.recv().unwrap_or(());
    });
    let (auth_tx, auth_rx) = std::sync::mpsc::channel();
    let working = Arc::clone(&hub);
    std::thread::spawn(move || {
        auth_tx
            .send(working.pairing.authenticate(&working, 2, &token.0))
            .unwrap();
    });
    fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&held_rx, "authenticate holds the pairing lock");
    let (revoke_tx, revoke_rx) = std::sync::mpsc::channel();
    let working = Arc::clone(&hub);
    std::thread::spawn(move || {
        revoke_tx
            .send(
                working
                    .pairing
                    .revoke(&working, &Who::Local, &args(json!({"device":"phone"}))),
            )
            .unwrap_or(());
    });
    release_tx.send(()).unwrap();
    let who = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&auth_rx, "authenticate registers")
        .unwrap();
    let revoked = match fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&revoke_rx, "revoke follows authenticate")
        .unwrap()
    {
        Outcome::Done(r) => r,
        Outcome::Dropped => panic!("local revoke dropped"),
    };
    assert!(who.revoked());
    finish_now(&hub, &clock, revoked);
}

#[test]
fn two_redemptions_of_one_code_admit_one() {
    let setup = Setup::new();
    let code = setup.mint("phone", &Who::Local);
    let hub = Arc::new(setup.hub);
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    hub.pairing.pause_in_lock(Pause::AfterFound, move || {
        held_tx.send(()).unwrap();
        release_rx.recv().unwrap_or(());
    });
    let (first_tx, first_rx) = std::sync::mpsc::channel();
    let working = Arc::clone(&hub);
    let first_code = code.clone();
    std::thread::spawn(move || {
        first_tx
            .send(
                working
                    .pairing
                    .redeem(&working, 1, &args(json!({"code":first_code})))
                    .is_ok(),
            )
            .unwrap();
    });
    fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&held_rx, "first redemption holds the lock");
    let (second_tx, second_rx) = std::sync::mpsc::channel();
    let working = Arc::clone(&hub);
    std::thread::spawn(move || {
        second_tx
            .send(matches!(
                working
                    .pairing
                    .redeem(&working, 2, &args(json!({"code":code}))),
                Err(Refused::Pairing)
            ))
            .unwrap();
    });
    release_tx.send(()).unwrap();
    assert!(
        fakes::Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&first_rx, "first redemption succeeds")
    );
    assert!(
        fakes::Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&second_rx, "second redemption refuses reuse")
    );
    assert_eq!(hub.pairing.live(), 1);
    assert_eq!(hub.pairing.pending(), 0);
}

#[test]
fn a_mint_that_waits_behind_its_issuers_revocation_mints_nothing() {
    let setup = Setup::new();
    let (_, who) = setup.pair("phone", 1);
    let clock = Arc::clone(&setup.clock);
    let hub = Arc::new(setup.hub);
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    hub.pairing
        .pause_in_lock(Pause::AfterRevokeMarked, move || {
            held_tx.send(()).unwrap();
            release_rx.recv().unwrap_or(());
        });
    let (rev_tx, rev_rx) = std::sync::mpsc::channel();
    let working = Arc::clone(&hub);
    std::thread::spawn(move || {
        rev_tx
            .send(
                working
                    .pairing
                    .revoke(&working, &Who::Local, &args(json!({"device":"phone"}))),
            )
            .unwrap_or(());
    });
    fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&held_rx, "revocation marks the issuer under the lock");
    let (mint_tx, mint_rx) = std::sync::mpsc::channel();
    let working = Arc::clone(&hub);
    std::thread::spawn(move || {
        mint_tx
            .send(matches!(
                working
                    .pairing
                    .mint(&working, &who, &args(json!({"device":"tablet"})))
                    .unwrap(),
                Outcome::Dropped
            ))
            .unwrap();
    });
    release_tx.send(()).unwrap();
    assert!(
        fakes::Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&mint_rx, "mint rechecks the issuer")
    );
    let revoked = match fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv_or_fail(&rev_rx, "revocation returns")
        .unwrap()
    {
        Outcome::Done(r) => r,
        Outcome::Dropped => panic!("local revoke dropped"),
    };
    assert_eq!(hub.pairing.pending(), 0);
    finish_now(&hub, &clock, revoked);
}

#[test]
fn a_failed_listener_registration_refuses_the_connection_and_keeps_the_notice() {
    use std::io::{BufRead as _, BufReader};
    let setup = Setup::new();
    let (_, who) = setup.pair("phone", 1);
    let hub = Arc::new(setup.hub);
    let (other_writer, other_peer) = UnixStream::pair().unwrap();
    other_peer
        .set_read_timeout(Some(MUST_SUCCEED_WITHIN))
        .unwrap();
    let closer_for = |writer: &UnixStream| crate::feed::Closer {
        stream: writer.try_clone().unwrap(),
        web: None,
    };
    let (closer, retired) = (closer_for(&other_writer), closer_for(&other_writer));
    let listener = hub
        .feed
        .attention
        .listen(Arc::new(Mutex::new(other_writer)), closer, retired)
        .unwrap();
    let (stream, mut peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
    hub.feed.attention.fail_next_listen();
    crate::connection::serve_authenticated(stream, Arc::clone(&hub), 1, who, None);
    let mut line = String::new();
    assert_eq!(BufReader::new(&mut peer).read_line(&mut line).unwrap(), 0);
    let mut reader = BufReader::new(other_peer);
    reader.read_line(&mut line).unwrap();
    let value: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["kind"], "device_changed");
    assert_eq!(value["payload"]["change"], "paired");
    assert_eq!(hub.pairing.live(), 0);
    assert!(hub.home.join("hub/devices/phone").exists());
    reader.get_ref().shutdown(Shutdown::Both).unwrap();
    hub.feed.attention.unlisten(listener);
}

#[test]
fn a_code_is_ten_characters_of_the_alphabet() {
    assert_eq!(
        code_from([0, 31, 32, 255, 1, 2, 3, 4, 5, 6]).0,
        "0z0z123456"
    );
}

#[test]
fn a_token_is_43_base64url_characters() {
    let bytes = std::array::from_fn(|i| u8::try_from(i).unwrap());
    assert_eq!(
        token_from(bytes).0,
        "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"
    );
    assert_eq!(format!("{:?}", token_from(bytes)), "DeviceToken(..)");
    assert_eq!(format!("{:?}", code_from([0; 10])), "PairingCode(..)");
}

#[test]
fn device_names_hold_to_the_rule() {
    for name in ["a", "A0_-.", "x.", &"a".repeat(63), &"a".repeat(64)] {
        assert!(DeviceName::parse(name).is_ok(), "{name}");
    }
    for name in ["", ".x", "..", "a/b", "local", "é", &"a".repeat(65)] {
        let refused = DeviceName::parse(name).err().unwrap();
        assert_eq!(refused.code, ErrorCode::InvalidArguments);
        assert_eq!(refused.message, NAME_RULE);
    }
}

#[test]
fn pairing_code_devices_and_revoke_reject_wrong_args() {
    for value in [
        json!({}),
        json!({"device":1}),
        json!({"device":"a", "extra":1}),
    ] {
        assert!(name(&args(value)).is_err());
    }
}
