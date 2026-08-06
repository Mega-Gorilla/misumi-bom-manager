// Excel link mode (docs/plans/0018-excel-link-mode/ plan.md §4 + implementation.md §2.1).
//
// Module layout follows implementation.md §2.1. Shipped so far: the persistence layer
// (`store`, schema V5 in db.rs — PR-1) and the pure logic ported from the PoC
// (`calc_state` / `fingerprint` / `env` — PR-2). Later PRs add: xlsx / contract /
// reader + link commands (PR-3), writeback / backup (PR-4), pending orchestration
// (PR-5), watch (PR-7). Orchestration lives here (mod.rs) from PR-3 on; until then
// this module is library code only and changes no app behavior.

pub mod calc_state;
pub mod env;
pub mod fingerprint;
pub mod store;
