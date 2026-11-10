use super::*;
use tokio::io::AsyncWriteExt;

fn command(value: &Value) -> &str {
    value.as_dictionary().unwrap()["Command"]
        .as_string()
        .unwrap()
}

fn pong() -> Value {
    dict(&[("Session", 2u64.into()), ("Command", "Pong".into())])
}

async fn handshake(peer: &mut DuplexStream) {
    assert_eq!(command(&outgoing(peer).await.0), "Ping");
    startup(peer).await;
}

async fn one_session(peer: &mut DuplexStream) {
    assert_eq!(command(&outgoing(peer).await.0), "RequestingSync");
    peer.write_all(&wire(&ready_message())).await.unwrap();
    assert_eq!(command(&outgoing(peer).await.0), "FinishedSyncingMetadata");
    peer.write_all(&wire(&incoming("SyncFinished", None)))
        .await
        .unwrap();
    peer.write_all(&wire(&allowed())).await.unwrap();
}

#[tokio::test]
async fn two_sessions_reuse_one_link_and_keep_separate_traces() {
    let link_trace = Temporary::new();
    let first = Temporary::new();
    let second = Temporary::new();
    let (client, mut peer) = transport(MAX_BYTES).await;
    let deadline = Instant::now() + Duration::from_secs(5);

    let device = async {
        handshake(&mut peer).await;
        one_session(&mut peer).await;
        one_session(&mut peer).await;
    };
    let host = async {
        let mut link = Link::from_atc(client, &link_trace.0, deadline);
        link.handshake(deadline).await.unwrap();
        let mut first = link
            .begin_sync(prepared(), &first.0, deadline)
            .await
            .unwrap();
        first.finish_metadata().await.unwrap();
        first.wait_metadata_only(&metadata_policy()).await.unwrap();
        let link = first.into_link().await.unwrap();
        let mut second = link
            .begin_sync(prepared(), &second.0, deadline)
            .await
            .unwrap();
        second.finish_metadata().await.unwrap();
        second.wait_metadata_only(&metadata_policy()).await.unwrap();
        second.into_link().await.unwrap()
    };
    tokio::join!(device, host);

    // The handshake ping is traced on the link, and each session restarts its
    // own numbering in its own directory.
    assert!(link_trace.0.join("000-outgoing.atc").is_file());
    assert!(first.0.join("000-outgoing.atc").is_file());
    assert!(second.0.join("000-outgoing.atc").is_file());
    assert!(!first.0.join("001-outgoing.atc").exists());
    assert!(!second.0.join("001-outgoing.atc").exists());
}

#[tokio::test]
async fn a_ping_between_sessions_is_answered_by_the_next_session() {
    let link_trace = Temporary::new();
    let first = Temporary::new();
    let second = Temporary::new();
    let (client, mut peer) = transport(MAX_BYTES).await;
    let deadline = Instant::now() + Duration::from_secs(5);

    let device = async {
        handshake(&mut peer).await;
        one_session(&mut peer).await;
        // Idle after the first session returned its link.
        peer.write_all(&wire(&incoming("Ping", None)))
            .await
            .unwrap();
        assert_eq!(command(&outgoing(&mut peer).await.0), "RequestingSync");
        assert_eq!(outgoing(&mut peer).await.0, pong());
        peer.write_all(&wire(&ready_message())).await.unwrap();
        assert_eq!(
            command(&outgoing(&mut peer).await.0),
            "FinishedSyncingMetadata"
        );
        peer.write_all(&wire(&incoming("SyncFinished", None)))
            .await
            .unwrap();
        peer.write_all(&wire(&allowed())).await.unwrap();
    };
    let host = async {
        let mut link = Link::from_atc(client, &link_trace.0, deadline);
        link.handshake(deadline).await.unwrap();
        let mut first = link
            .begin_sync(prepared(), &first.0, deadline)
            .await
            .unwrap();
        first.finish_metadata().await.unwrap();
        first.wait_metadata_only(&metadata_policy()).await.unwrap();
        let link = first.into_link().await.unwrap();
        let mut second = link
            .begin_sync(prepared(), &second.0, deadline)
            .await
            .unwrap();
        second.finish_metadata().await.unwrap();
        second.wait_metadata_only(&metadata_policy()).await.unwrap();
        second.into_link().await.unwrap();
    };
    tokio::join!(device, host);
}

#[tokio::test]
async fn a_link_that_closes_between_sessions_is_not_returned() {
    let link_trace = Temporary::new();
    let first = Temporary::new();
    let (client, mut peer) = transport(MAX_BYTES).await;
    let deadline = Instant::now() + Duration::from_secs(5);

    let device = async {
        handshake(&mut peer).await;
        assert_eq!(command(&outgoing(&mut peer).await.0), "RequestingSync");
        peer.write_all(&wire(&ready_message())).await.unwrap();
        assert_eq!(
            command(&outgoing(&mut peer).await.0),
            "FinishedSyncingMetadata"
        );
        peer.write_all(&wire(&incoming("SyncFinished", None)))
            .await
            .unwrap();
        // No SyncAllowed: the phone closed the link (IdleExit).
        drop(peer);
    };
    let host = async {
        let mut link = Link::from_atc(client, &link_trace.0, deadline);
        link.handshake(deadline).await.unwrap();
        let mut first = link
            .begin_sync(prepared(), &first.0, deadline)
            .await
            .unwrap();
        first.finish_metadata().await.unwrap();
        first.wait_metadata_only(&metadata_policy()).await.unwrap();
        assert!(first.into_link().await.is_err());
    };
    tokio::join!(device, host);
}

#[tokio::test]
async fn a_session_that_did_not_finish_cannot_return_its_link() {
    let link_trace = Temporary::new();
    let first = Temporary::new();
    let (client, mut peer) = transport(MAX_BYTES).await;
    let deadline = Instant::now() + Duration::from_secs(5);

    let device = async {
        handshake(&mut peer).await;
        assert_eq!(command(&outgoing(&mut peer).await.0), "RequestingSync");
        peer.write_all(&wire(&ready_message())).await.unwrap();
        assert_eq!(
            command(&outgoing(&mut peer).await.0),
            "FinishedSyncingMetadata"
        );
        // A terminal the metadata-only wait refuses.
        peer.write_all(&wire(&incoming("SyncFailed", None)))
            .await
            .unwrap();
        let mut byte = [0];
        let _ = tokio::io::AsyncReadExt::read(&mut peer, &mut byte).await;
    };
    let host = async {
        let mut link = Link::from_atc(client, &link_trace.0, deadline);
        link.handshake(deadline).await.unwrap();
        let mut first = link
            .begin_sync(prepared(), &first.0, deadline)
            .await
            .unwrap();
        first.finish_metadata().await.unwrap();
        assert!(first.wait_metadata_only(&metadata_policy()).await.is_err());
        // The link is consumed with the session; nothing is handed back.
        assert!(first.into_link().await.is_err());
    };
    tokio::join!(device, host);
}

#[tokio::test]
async fn an_idle_link_answers_a_ping_and_the_wait_ends_at_the_window() {
    let link_trace = Temporary::new();
    let (client, mut peer) = transport(MAX_BYTES).await;
    let deadline = Instant::now() + Duration::from_secs(5);

    let device = async {
        handshake(&mut peer).await;
        peer.write_all(&wire(&incoming("Ping", None)))
            .await
            .unwrap();
        assert_eq!(command(&outgoing(&mut peer).await.0), "Pong");
        // The link stays open past the host's window.
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    let host = async {
        let mut link = Link::from_atc(client, &link_trace.0, deadline);
        link.handshake(deadline).await.unwrap();
        assert!(!link.wait_lost(Duration::from_millis(200)).await);
    };
    tokio::join!(device, host);
}

#[tokio::test]
async fn a_refresh_notice_does_not_end_the_wait_before_the_window() {
    let link_trace = Temporary::new();
    let (client, mut peer) = transport(MAX_BYTES).await;
    let deadline = Instant::now() + Duration::from_secs(5);

    let device = async {
        handshake(&mut peer).await;
        // The phone finishes its refresh and pushes new metrics; the port
        // stall follows the notice, so the notice must not end the wait.
        peer.write_all(&wire(&incoming("AssetMetrics", Some(dict(&[])))))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
    };
    let host = async {
        let mut link = Link::from_atc(client, &link_trace.0, deadline);
        link.handshake(deadline).await.unwrap();
        assert!(!link.wait_lost(Duration::from_millis(200)).await);
    };
    tokio::join!(device, host);
}

#[tokio::test]
async fn a_stall_ends_the_wait_before_the_window() {
    let link_trace = Temporary::new();
    let (client, mut peer) = transport(MAX_BYTES).await;
    let deadline = Instant::now() + Duration::from_secs(5);

    let device = async {
        handshake(&mut peer).await;
        // The phone stalls the port: the link closes with no message.
        drop(peer);
    };
    let host = async {
        let mut link = Link::from_atc(client, &link_trace.0, deadline);
        link.handshake(deadline).await.unwrap();
        assert!(link.wait_lost(Duration::from_secs(5)).await);
    };
    tokio::join!(device, host);
}

#[tokio::test]
async fn an_unsent_request_can_go_out_on_a_new_link() {
    let first_trace = Temporary::new();
    let second_trace = Temporary::new();
    let device_trace = Temporary::new();
    let (closed, peer) = transport(MAX_BYTES).await;
    drop(peer);
    let deadline = Instant::now() + Duration::from_secs(5);

    let (error, prepared) = Link::from_atc(closed, &first_trace.0, deadline)
        .begin_sync_or_unsent(prepared(), &first_trace.0, deadline)
        .await
        .err()
        .expect("a link with no phone cannot send");
    assert!(!format!("{error:#}").is_empty());
    let prepared = prepared.expect("the request was never sent");

    let (client, mut peer) = transport(MAX_BYTES).await;
    let device = async {
        handshake(&mut peer).await;
        one_session(&mut peer).await;
    };
    let host = async {
        let mut link = Link::from_atc(client, &device_trace.0, deadline);
        link.handshake(deadline).await.unwrap();
        let mut session = link
            .begin_sync(prepared, &second_trace.0, deadline)
            .await
            .unwrap();
        session.finish_metadata().await.unwrap();
        session
            .wait_metadata_only(&metadata_policy())
            .await
            .unwrap();
        session.into_link().await.unwrap();
    };
    tokio::join!(device, host);
}
