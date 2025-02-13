//! Integration test for the curp server

use std::{sync::Arc, time::Duration};

use utils::config::ClientTimeout;

use crate::common::{
    curp_group::{proto::propose_response::ExeResult, CurpGroup, ProposeRequest, ProposeResponse},
    init_logger, sleep_millis, sleep_secs,
    test_cmd::TestCommand,
};

mod common;

#[tokio::test]
async fn basic_propose() {
    init_logger();

    let group = CurpGroup::new(3).await;
    let client = group.new_client(ClientTimeout::default()).await;

    assert_eq!(
        client
            .propose(TestCommand::new_put(vec![0], 0))
            .await
            .unwrap(),
        vec![]
    );
    assert_eq!(
        client.propose(TestCommand::new_get(vec![0])).await.unwrap(),
        vec![0]
    );

    group.stop();
}

#[tokio::test]
async fn synced_propose() {
    init_logger();

    let mut group = CurpGroup::new(5).await;
    let client = group.new_client(ClientTimeout::default()).await;
    let cmd = TestCommand::new_get(vec![0]);

    let (er, index) = client.propose_indexed(cmd.clone()).await.unwrap();
    assert_eq!(er, vec![]);
    assert_eq!(index, 1); // log[0] is a fake one

    for exe_rx in group.exe_rxs() {
        let (cmd1, er) = exe_rx.recv().await.unwrap();
        assert_eq!(cmd1, cmd);
        assert_eq!(er, vec![]);
    }

    for as_rx in group.as_rxs() {
        let (cmd1, index) = as_rx.recv().await.unwrap();
        assert_eq!(cmd1, cmd);
        assert_eq!(index, 1);
    }

    group.stop();
}

// Each command should be executed once and only once on each node
#[tokio::test]
async fn exe_exact_n_times() {
    init_logger();

    let mut group = CurpGroup::new(3).await;
    let client = group.new_client(ClientTimeout::default()).await;
    let cmd = TestCommand::new_get(vec![0]);

    let er = client.propose(cmd.clone()).await.unwrap();
    assert_eq!(er, vec![]);

    for exe_rx in group.exe_rxs() {
        let (cmd1, er) = exe_rx.recv().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), exe_rx.recv())
                .await
                .is_err()
        );
        assert_eq!(cmd1, cmd);
        assert_eq!(er, vec![]);
    }

    for as_rx in group.as_rxs() {
        let (cmd1, index) = as_rx.recv().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), as_rx.recv())
                .await
                .is_err()
        );
        assert_eq!(cmd1, cmd);
        assert_eq!(index, 1);
    }

    group.stop();
}

// To verify PR #86 is fixed
#[tokio::test]
async fn fast_round_is_slower_than_slow_round() {
    init_logger();

    let group = CurpGroup::new(3).await;
    let cmd = Arc::new(TestCommand::new_get(vec![0]));

    let leader = group.get_leader().await.0;

    // send propose only to the leader
    let mut leader_connect = group.get_connect(&leader).await;
    leader_connect
        .propose(tonic::Request::new(ProposeRequest {
            command: bincode::serialize(&cmd).unwrap(),
        }))
        .await
        .unwrap();

    // wait for the command to be synced to others
    // because followers never get the cmd from the client, it will mark the cmd done in spec pool instead of removing the cmd from it
    tokio::time::sleep(Duration::from_secs(1)).await;

    // send propose to follower
    let follower_addr = group.all.keys().find(|&id| &leader != id).unwrap();
    let mut follower_connect = group.get_connect(follower_addr).await;

    // the follower should response empty immediately
    let resp: ProposeResponse = follower_connect
        .propose(tonic::Request::new(ProposeRequest {
            command: bincode::serialize(&cmd).unwrap(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.exe_result.is_none());

    group.stop();
}

#[tokio::test]
async fn concurrent_cmd_order() {
    init_logger();

    let cmd0 = TestCommand::new_put(vec![0], 0).set_exe_dur(Duration::from_secs(1));
    let cmd1 = TestCommand::new_put(vec![0, 1], 1);
    let cmd2 = TestCommand::new_put(vec![1], 2);

    let group = CurpGroup::new(3).await;
    let leader = group.get_leader().await.0;
    let mut leader_connect = group.get_connect(&leader).await;

    let mut c = leader_connect.clone();
    tokio::spawn(async move {
        c.propose(ProposeRequest {
            command: bincode::serialize(&cmd0).unwrap(),
        })
        .await
        .expect("propose failed");
    });

    sleep_millis(20).await;
    let response = leader_connect
        .propose(ProposeRequest {
            command: bincode::serialize(&cmd1).unwrap(),
        })
        .await
        .expect("propose failed")
        .into_inner();
    assert!(matches!(response.exe_result.unwrap(), ExeResult::Error(_)));
    let response = leader_connect
        .propose(ProposeRequest {
            command: bincode::serialize(&cmd2).unwrap(),
        })
        .await
        .expect("propose failed")
        .into_inner();
    assert!(matches!(response.exe_result.unwrap(), ExeResult::Error(_)));

    sleep_secs(1).await;

    let client = group.new_client(ClientTimeout::default()).await;

    assert_eq!(
        client.propose(TestCommand::new_get(vec![1])).await.unwrap(),
        vec![2]
    );

    group.stop();
}
