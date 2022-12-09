// Copyright 2022 TiKV Project Authors. Licensed under Apache-2.0.

use std::{assert_matches::assert_matches, time::Duration};

use engine_traits::Peekable;
use futures::executor::block_on;
use kvproto::{
    raft_cmdpb::{CmdType, Request},
    raft_serverpb::RaftMessage,
};
use raftstore::store::{INIT_EPOCH_CONF_VER, INIT_EPOCH_VER};
use raftstore_v2::router::PeerMsg;
use tikv_util::store::new_peer;

use crate::cluster::Cluster;

/// Test basic write flow.
#[test]
fn test_basic_write() {
    let cluster = Cluster::default();
    let router = &cluster.routers[0];
    let mut req = router.new_request_for(2);
    let mut put_req = Request::default();
    put_req.set_cmd_type(CmdType::Put);
    put_req.mut_put().set_key(b"key".to_vec());
    put_req.mut_put().set_value(b"value".to_vec());
    req.mut_requests().push(put_req);

    router.wait_applied_to_current_term(2, Duration::from_secs(3));

    // Good proposal should be committed.
    let (msg, mut sub) = PeerMsg::raft_command(req.clone());
    router.send(2, msg).unwrap();
    assert!(block_on(sub.wait_proposed()));
    assert!(block_on(sub.wait_committed()));
    let resp = block_on(sub.result()).unwrap();
    assert!(!resp.get_header().has_error(), "{:?}", resp);

    // Store id should be checked.
    let mut invalid_req = req.clone();
    invalid_req.mut_header().set_peer(new_peer(3, 3));
    let resp = router.command(2, invalid_req.clone()).unwrap();
    assert!(
        resp.get_header().get_error().has_store_not_match(),
        "{:?}",
        resp
    );

    // Peer id should be checked.
    let mut invalid_req = req.clone();
    invalid_req.mut_header().set_peer(new_peer(1, 1));
    let resp = router.command(2, invalid_req.clone()).unwrap();
    assert!(resp.get_header().has_error(), "{:?}", resp);

    // Epoch should be checked.
    let mut invalid_req = req.clone();
    invalid_req
        .mut_header()
        .mut_region_epoch()
        .set_version(INIT_EPOCH_VER - 1);
    let resp = router.command(2, invalid_req.clone()).unwrap();
    assert!(
        resp.get_header().get_error().has_epoch_not_match(),
        "{:?}",
        resp
    );

    // It's wrong to send query to write command.
    let mut invalid_req = req.clone();
    let mut snap_req = Request::default();
    snap_req.set_cmd_type(CmdType::Snap);
    invalid_req.mut_requests().push(snap_req);
    let resp = router.command(2, invalid_req.clone()).unwrap();
    assert!(resp.get_header().has_error(), "{:?}", resp);

    // Term should be checked if set.
    let mut invalid_req = req.clone();
    invalid_req.mut_header().set_term(1);
    let resp = router.command(2, invalid_req).unwrap();
    assert!(
        resp.get_header().get_error().has_stale_command(),
        "{:?}",
        resp
    );

    // Too large message can cause regression and should be rejected.
    let mut invalid_req = req.clone();
    invalid_req.mut_requests()[0]
        .mut_put()
        .set_value(vec![0; 8 * 1024 * 1024]);
    let resp = router.command(2, invalid_req).unwrap();
    assert!(
        resp.get_header().get_error().has_raft_entry_too_large(),
        "{:?}",
        resp
    );

    // Make it step down and follower should reject write.
    let mut msg = Box::<RaftMessage>::default();
    msg.set_region_id(2);
    msg.set_to_peer(new_peer(1, 3));
    msg.mut_region_epoch().set_conf_ver(INIT_EPOCH_CONF_VER);
    msg.set_from_peer(new_peer(2, 4));
    let raft_message = msg.mut_message();
    raft_message.set_msg_type(raft::prelude::MessageType::MsgHeartbeat);
    raft_message.set_from(4);
    raft_message.set_term(8);
    router.send_raft_message(msg).unwrap();
    let resp = router.command(2, req).unwrap();
    assert!(resp.get_header().get_error().has_not_leader(), "{:?}", resp);
}

#[test]
fn test_put_delete() {
    let cluster = Cluster::default();
    let router = &cluster.routers[0];
    let mut req = router.new_request_for(2);
    let mut put_req = Request::default();
    put_req.set_cmd_type(CmdType::Put);
    put_req.mut_put().set_key(b"key".to_vec());
    put_req.mut_put().set_value(b"value".to_vec());
    req.mut_requests().push(put_req);

    router.wait_applied_to_current_term(2, Duration::from_secs(3));

    let registry = cluster.node(0).tablet_registry();
    let tablet = registry.get(2).unwrap().latest().unwrap().clone();
    assert!(tablet.get_value(b"key").unwrap().is_none());
    let (msg, mut sub) = PeerMsg::raft_command(req.clone());
    router.send(2, msg).unwrap();
    assert!(block_on(sub.wait_proposed()));
    assert!(block_on(sub.wait_committed()));
    let resp = block_on(sub.result()).unwrap();
    assert!(!resp.get_header().has_error(), "{:?}", resp);
    assert_eq!(tablet.get_value(b"key").unwrap().unwrap(), b"value");

    let mut delete_req = Request::default();
    delete_req.set_cmd_type(CmdType::Delete);
    delete_req.mut_delete().set_key(b"key".to_vec());
    req.clear_requests();
    req.mut_requests().push(delete_req);
    let (msg, mut sub) = PeerMsg::raft_command(req.clone());
    router.send(2, msg).unwrap();
    assert!(block_on(sub.wait_proposed()));
    assert!(block_on(sub.wait_committed()));
    let resp = block_on(sub.result()).unwrap();
    assert!(!resp.get_header().has_error(), "{:?}", resp);
    assert_matches!(tablet.get_value(b"key"), Ok(None));
}
