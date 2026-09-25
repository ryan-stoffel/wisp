//! Ids for agent runs and their turns (0007 and 0011). The `agents` capability's methods come in
//! M3; wispd's backends use these ids already.

use crate::id::uuid_v7_id;

uuid_v7_id! {
    /// An agent run's id: a version 7 UUID that the client generates once and sends again on every
    /// retry of `agent/start`, so a retry never starts a second agent.
    RunId
}

uuid_v7_id! {
    /// A follow-up turn's id: a version 7 UUID that the client generates once and sends again on
    /// every retry of `agent/send`, so a retry never sends the message twice.
    TurnId
}
