use serde_json::json;
use crate::PresenceHub;

#[tokio::test]
async fn hub_subscribes_broadcasts_and_unsubscribes() {
    let hub = PresenceHub::new();
    let (id1, mut rx1) = hub.subscribe();
    let (id2, mut rx2) = hub.subscribe();
    assert_ne!(id1, id2);

    let hello1 = rx1.recv().await.unwrap();
    assert_eq!(hello1["type"], "hello");
    assert_eq!(hello1["id"], id1);
    let hello2 = rx2.recv().await.unwrap();
    assert_eq!(hello2["id"], id2);

    hub.broadcast(&json!({ "type": "cursor", "from": id1, "data": {"x": 1} }));
    let m1 = rx1.recv().await.unwrap();
    let m2 = rx2.recv().await.unwrap();
    assert_eq!(m1["from"], id1);
    assert_eq!(m2["from"], id1);

    hub.unsubscribe(id1);
    hub.broadcast(&json!({ "type": "cursor", "from": id2, "data": {"x": 2} }));
    let m = rx2.recv().await.unwrap();
    assert_eq!(m["from"], id2);
    assert!(rx1.try_recv().is_err(), "已退房不应再收到广播");
}
