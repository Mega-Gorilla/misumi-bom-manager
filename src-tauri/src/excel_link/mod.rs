// Excel link mode (docs/plans/0018-excel-link-mode/ plan.md §4 + implementation.md §2.1).
//
// Module layout follows implementation.md §2.1. PR-1 ships only the persistence layer
// (`store`, schema V5 in db.rs). Later PRs add: calc_state / fingerprint / env (PR-2),
// xlsx / contract / reader + link commands (PR-3), writeback / backup (PR-4),
// pending orchestration (PR-5), watch (PR-7). Orchestration lives here (mod.rs) from
// PR-3 on; until then this module is data-access only and changes no app behavior.

pub mod store;
