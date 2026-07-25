{
  "source": "https://github.com/rust-lang/this-week-in-rust/pull/8300#issuecomment-4861715103",
  "files": ["case.md"],
  "warnings": [
    "community list item has text outside its link; include the description inside the link title",
    "community list item is unusually long; use a concise link title"
  ]
}
---
## Updates from Rust Community

### Project/Tooling Updates

* [mqtt-typed-client 0.2](https://holovskyi.github.io/blog/typed-mqtt-topics-for-rust/) - a type-safe async MQTT client on top of rumqttc: declare topics as structs with `#[mqtt_topic("...")]` and get typed publish/subscribe, parameter parsing via FromStr, and tree-based routing instead of hand-written `format!()` and `split('/')`.
