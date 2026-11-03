//! The connection banner words an incompatible core by direction (PROTO-4), and QML has no way to
//! read the app's own protocol major yet, so the banner holds it as `appMajor`. This pins that
//! constant to the real one: a protocol major bump fails here until the banner follows.

use lil_protocol::session::PROTOCOL;

const BANNER: &str = include_str!("../../../apps/lil-ui-kit/qml/shell/ConnectionBanner.qml");

#[test] // spec: APP-036, ARCH-023
fn the_banners_app_major_is_the_protocols() {
    let wanted = format!("readonly property int appMajor: {}", PROTOCOL.major);
    assert!(
        BANNER.contains(&wanted),
        "ConnectionBanner.qml must hold `{wanted}` (lil_protocol::session::PROTOCOL.major)"
    );
}
