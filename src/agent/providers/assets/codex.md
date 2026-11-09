Codex keeps its login in `auth.json` and its preferences in `config.toml`.
The login belongs to this identity on this machine. Sessions, runtime databases,
shell snapshots and writer locks belong to each task's capsule.
altitude supplies its developer instructions at the start of every autonomous
turn and connects its tools through the managed `altitude-bridge.py`.
