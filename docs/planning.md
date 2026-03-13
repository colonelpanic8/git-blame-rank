# git-blame-rank Planning

## Goal

Build a Rust tool in the spirit of `git-fame-rb`, but optimized for:

- incremental results while the scan is still running
- parallel processing across many files
- an interactive TUI for path-based drill-down
- a design that can scale to large repositories without turning the UI into the bottleneck

## Product Shape

The first version should be a terminal app with two modes:

1. `tui` mode
   - Default interactive mode.
   - Shows repo progress immediately.
   - Updates author rankings as files finish.
   - Lets the user navigate a repo tree and inspect any directory or file while the scan is still in flight.
2. `report` mode
   - Non-interactive CLI output for scripting and debugging.
   - Shares the same backend pipeline as the TUI.

The key product decision is that "drill down" means path-based exploration first:

- repo root
- directory
- subdirectory
- file

That is enough to make the tool genuinely useful without requiring a full commit browser or hunk explorer in v1.

## Recommended Technical Direction

Use `git2` as the primary repository and blame backend, and build the incremental UI, aggregation, and caching in Rust.

This is the right starting point if we want a coherent in-process architecture:

- revision resolution, tree walking, and blame stay inside one Rust API
- we avoid subprocess management and stdout parsing
- the backend can return compact Rust-native summaries
- later features like inspecting modified content are easier because `git2` exposes `blame_buffer`

The key limitation is that `git2` does not expose a streaming blame API analogous to `git blame --incremental`.

That means the natural incremental unit is the completed file, not the blame hunk. That is still a good fit for the product as long as we process many files in parallel and update the UI as each file finishes.

## Why `git2` First

### What `git2` Gives Us

`git2` already exposes the primitives we need:

- open repositories and resolve revisions
- walk trees to enumerate files at a commit
- run blame for a file with configurable options
- inspect blame hunks and the final commit responsible for each hunk

That is enough for a clean first implementation without maintaining a subprocess protocol.

### The Tradeoffs

`git2` is not free:

- it brings `libgit2` into the dependency chain
- some edge-case blame behavior can differ from native Git
- blame results are returned only after the file finishes

I still think this is the better first implementation if the priority is a solid Rust architecture with a responsive UI.

### Why Not Make `gix` Primary Yet

`gix` is promising and worth keeping in mind, but it is not the shortest path to a working interactive product.

- `git2` is the more established in-process option for this exact task
- the product risk is in aggregation and UI design more than backend novelty
- a backend trait gives us room to revisit `gix` later

### Keep a Backend Escape Hatch

We should still define a backend trait from the start.

If `git2` turns out to have correctness gaps that matter on real repositories, we can add a native-`git` compatibility backend later without redesigning the rest of the application.

## High-Level Architecture

The architecture should be a single-owner UI state machine fed by worker threads.

```text
CLI args
  -> Repo open / revision resolution
  -> File enumerator
  -> Work queue
  -> N blame workers
  -> Event channel
  -> Main event loop
       - merge file results
       - update progress
       - refresh selected-scope cache
       - draw TUI
```

### Core Principle

The UI thread should own all mutable app state.

Workers only do this:

- receive a file path
- run `git2` blame for that file
- convert the result into a compact summary
- send the summary back over a channel

This avoids shared mutable state, lock contention, and TUI synchronization bugs.

## Execution Model in Detail

This should be event-driven, but not async-runtime-driven.

Use:

- one main thread for UI, control decisions, and state ownership
- a fixed worker pool for blame execution
- one bounded work queue for jobs
- one unbounded or generously bounded event queue for results and status updates
- a redraw tick on the main thread, for example every 50-100ms

The important distinction is:

- workers do data-plane work
- the main thread owns the control plane

That means the main thread decides:

- which files are eligible to run
- which jobs are currently in flight
- which results are still relevant
- when cached aggregates should be invalidated or rebuilt

### Why No Tokio

We do need asynchronous behavior in the product sense, but not Rust `async` in the runtime sense.

The hard work here is:

- CPU-heavy blame processing
- repository I/O
- TUI event handling

That maps cleanly to threads plus channels. Using Tokio would complicate the application without improving the core scheduling or aggregation problems.

## File Lifecycle and Scheduler

Every discovered file should have an explicit lifecycle entry in app state.

```rust
enum FileStatus {
    Pending,
    Queued { generation: u64 },
    Running { generation: u64, worker_id: usize, started_at: Instant },
    Complete { generation: u64, summary_id: SummaryId },
    Failed { generation: u64, error: Arc<str> },
    Stale,
}
```

The scheduler should operate on generations.

A generation is a scan plan derived from:

- revision
- blame options
- active filters
- active include/exclude file selections

Whenever one of those inputs changes materially, increment `generation_id` and produce a new plan.

That gives us a clean rule:

- results from older generations are ignored unless we explicitly choose to merge them

This is the simplest way to make future dynamic filtering safe.

### Initial Scheduling Policy

For v1, queue all eligible files immediately for the active generation, up to the worker pool capacity.

As workers free up:

- pop the next eligible file from the pending queue
- mark it `Running`
- send a `WorkItem` to a worker

Suggested queue ordering:

- largest files first if we can estimate size cheaply
- otherwise lexical order is fine

Large-first is often better for perceived responsiveness because the long tail starts earlier.

### Work Item Shape

```rust
struct WorkItem {
    generation: u64,
    rev: Arc<str>,
    path: BString,
    filter_snapshot: FilterSnapshot,
    blame_config: BlameConfig,
}
```

`filter_snapshot` is mostly future-proofing. Even if we do not need it in the worker initially, having the work item be self-contained will make dynamic controls easier later.

## Result Reintegration

Workers should never mutate shared aggregates directly.

They return compact, owned results:

```rust
enum WorkerEvent {
    Started {
        generation: u64,
        path: BString,
        worker_id: usize,
    },
    Finished {
        generation: u64,
        path: BString,
        summary: FileSummary,
        elapsed: Duration,
    },
    Failed {
        generation: u64,
        path: BString,
        error: Arc<str>,
        elapsed: Duration,
    },
}
```

The main thread consumes these events and reintegrates them in a fixed order:

1. discard stale-generation events
2. update per-file lifecycle state
3. store or replace the file summary
4. update tree progress counters
5. update root aggregate incrementally
6. patch any hot scope caches that currently include that path
7. mark visible tables dirty

This is the core rule that keeps the system coherent:

- all aggregate mutation happens on the main thread

## Aggregation State Model

There should be two distinct aggregation layers.

### 1. Durable Per-File Storage

This is the source of truth.

```rust
struct FileRecord {
    path: BString,
    ext: Option<SmolStr>,
    status: FileStatus,
    summary: Option<FileSummary>,
}
```

The scheduler and filters operate over `FileRecord`s.

The main thread never needs to ask a worker for old information again if the `FileSummary` is already available and still relevant.

### 2. Derived Scope Aggregates

These are computed views.

```rust
struct ScopeAggregate {
    scope: ScopeKey,
    generation: u64,
    included_files: u32,
    completed_files: u32,
    author_stats: Vec<AuthorAggregate>,
    warnings: Vec<ScopeWarning>,
    dirty: bool,
}
```

The important modeling choice is:

- `FileSummary` is stored once
- `ScopeAggregate` is rebuilt or patched from stored file summaries

That keeps reintegration cheap and makes filter changes manageable.

## How Sums Get Updated

The root aggregate should be maintained incrementally at all times.

When a completed file arrives:

- subtract the previous contribution for that file if we are replacing an older summary for the same generation
- add the new file contribution into the root aggregate
- update root author rows by author id

For non-root scopes, use a hybrid strategy:

- selected scope gets live incremental patching
- cached scopes get patched opportunistically if they are already hot
- uncached scopes are rebuilt on selection

### Per-File Contribution Strategy

Think of each file as contributing a compact delta:

```rust
struct FileContribution {
    total_lines: u32,
    total_files: u32, // almost always 1
    per_author: Vec<AuthorContribution>,
}
```

Then root updates become:

- remove old contribution if present
- add new contribution

This is cleaner than trying to mutate aggregates directly from raw hunk data.

### Commit Counting

For `lines` and `files`, per-file deltas are trivial.

For `commits`, use:

- exact distinct commit sets in the root aggregate
- exact distinct commit sets in the selected scope aggregate
- lazy rebuilds for other scopes

That means `AuthorAggregate` likely wants two representations:

- a display projection with counts
- an internal commit set used only where exactness is required

## Control Plane for Dynamic Filtering

Dynamic filtering should be designed in now, even if the first UI only exposes a small subset.

The filter model should be composable:

```rust
struct FilterState {
    included_paths: PathFilterSet,
    excluded_paths: PathFilterSet,
    file_types: BTreeSet<SmolStr>,
    text_query: Option<String>,
    only_selected_tree: Option<NodeId>,
    show_completed_only: bool,
}
```

The important distinction is between:

- scan eligibility: should this file be processed at all?
- view eligibility: should this file contribute to the currently displayed aggregate?

For v1, we can keep those coupled.

For the future TUI, we should decouple them:

- the scan may continue to collect summaries for many files
- the current view may include only a subset of those files

That opens the door to checkbox-style file inclusion in the TUI without forcing a rescan for every display change.

### Recommended Future Filtering UX

Not necessary for the first implementation, but the data model should support:

- toggling individual files or directories in or out of the active view
- restricting to one or more file extensions
- restricting to the currently focused subtree
- quick text filter over path names

The cleanest TUI model is a small filter bar or modal that edits `FilterState`, then triggers:

1. a generation bump if scan eligibility changed
2. a scope-cache invalidation for affected views
3. a recompute of the visible aggregate from stored file summaries

## Event Loop Responsibilities

The main loop should multiplex three sources:

- terminal input events
- worker events
- periodic redraw ticks

Pseudo-flow:

```text
loop:
  drain worker events up to a budget
  process pending UI input
  schedule more work if capacity is available
  rebuild dirty visible aggregates if needed
  draw on tick or when forced
```

Two practical rules matter here:

- drain worker events in batches instead of one redraw per event
- cap rebuild work per loop iteration so navigation stays responsive

If a large scope rebuild is needed, break it into chunks and finish it over multiple ticks.

## Data Flow

### 1. Repository Setup

- open the repo with `git2::Repository`
- resolve the target revision, default `HEAD`
- obtain the commit tree for that revision
- enumerate blobs from the tree
- optionally apply user path filters before queueing work

For v1, analyze committed files at a revision, not arbitrary dirty working tree content.

That keeps behavior deterministic and blame-compatible.

### 2. Worker Execution

Each worker runs the equivalent of:

```rust
repo.blame_file(path, Some(&mut blame_options))
```

Options we should expose early:

- ignore whitespace changes
- detect moved lines within a file
- detect copied or moved lines across files
- revision target selection

Ignored revisions support needs specific validation. If `git2` support is incomplete for the behavior we want, that can become a later compatibility item.

### 3. File Summary

Each completed file should produce something like:

```rust
struct FileSummary {
    path: BString,
    total_lines: u32,
    authors: Vec<FileAuthorStat>,
    warnings: Vec<FileWarning>,
}

struct FileAuthorStat {
    author_id: AuthorId,
    lines: u32,
    commit_ids: Vec<CommitId>,
}
```

Important point: the file summary is the durable unit of aggregation.

Do not keep full per-line blame state for all files in memory.

Also do not pass `git2::Blame` objects around the program. Convert them immediately into owned Rust data. The `git2` blame types are not the shape we want for cross-thread application state.

## Blame Extraction Strategy

Use `git2` blame hunks directly.

The extractor should:

- iterate `BlameHunk`s
- resolve commit metadata for the final commit id of each hunk
- map each hunk to an `AuthorId`
- accumulate line counts and distinct commit ids by author

Do not retain the hunk list after summarization.

We want a compact extractor that produces a `FileSummary` as soon as a file finishes.

## Drill-Down Model

The tree on the left should represent repository paths and progress, not full precomputed author aggregates for every node.

That is an intentional scalability choice.

### Tree State Stores

Each node should eagerly track:

- path / name
- children
- total files under node
- completed files under node
- warning count
- expanded/collapsed state

### Scope Aggregates

Author stats for a selected path should be computed from completed descendant `FileSummary` values and cached.

This is the key design that makes dynamic drill-down work without exploding memory:

- root aggregate is maintained incrementally all the time
- selected-node aggregate is maintained incrementally while selected
- recently visited nodes can keep an LRU cache
- selecting an uncached node triggers a rebuild from stored file summaries

This gives us:

- immediate incremental root rankings
- responsive path drill-down
- exact commit counts for the selected scope
- bounded memory growth

It also avoids storing a `HashMap<Author, Stats>` for every directory in a large repository.

## Why Commit Counts Need Special Handling

`lines` and `files` are cheap.

Exact `commits` per author per path are not cheap if we eagerly maintain them for every node in the tree, because that requires distinct-set tracking across many overlapping scopes.

The right tradeoff is:

- keep `lines` and `files` easy everywhere
- keep exact `commits` in scope caches only
- compute or refresh those caches when the user selects a node

This keeps the live root view fast and still gives correct numbers where the user is actually looking.

## TUI Design

Recommended initial layout:

```text
+--------------------------------------------------------------+
| repo  rev  jobs  processed/total  warnings  elapsed          |
+---------------------------+----------------------------------+
| path tree                 | author table                     |
|                           |                                  |
| src/        120/400       | author     lines files commits   |
|   core/      40/80        | Alice      1234    53     41     |
|   tui/       10/60        | Bob         980    44     37     |
|   git/       70/260       | ...                              |
|                           |                                  |
+---------------------------+----------------------------------+
| status / key hints                                           |
+--------------------------------------------------------------+
```

Recommended key model:

- `j` / `k` or arrows: move selection
- `h` / `l`: collapse / expand
- `Enter`: zoom selected path into focus
- `Backspace`: zoom out
- `Tab`: switch right-pane views
- `q`: quit

Right-pane views for v1:

1. `Authors`
2. `Files`
3. `Warnings`

`Authors` is the main view.

`Files` should show processed files within the selected scope, sorted by line count or completion order.

### Future Interactive Controls

The TUI should eventually support lightweight filter editing in-place.

Suggested keys:

- `Space`: include or exclude the selected file or directory from the active view filter
- `f`: open a small filter panel
- `x`: cycle file-extension filters
- `/`: text filter by path

This should not require redesigning the backend if we keep `FilterState`, generation ids, and per-file summaries as separate concepts from the start.

## Backend Trait Boundary

Even though v1 should use `git2`, define a backend interface early.

```rust
trait BlameBackend {
    fn discover_files(&self, rev: &str, path_filters: &[BString]) -> Result<Vec<BString>>;
    fn blame_file(&self, rev: &str, path: &BStr) -> Result<FileSummary>;
}
```

Start with:

- `Git2Backend`

Possible later additions:

- `GitCliBackend`
- `GixBackend`
- serialized cache / on-disk snapshot backend

This trait boundary prevents the TUI and aggregation code from depending directly on `git2`.

## Identity Handling

Author identity should default to:

- author name
- author email

interned into a stable `AuthorId`.

Display policy:

- primary label: author name
- secondary label: author email

Later options:

- mailmap normalization
- email-only or name-only grouping
- configurable alias collapsing

## Concurrency Model

Use plain worker threads with `crossbeam-channel`, not Tokio.

Reasons:

- the expensive work is `git2` blame and aggregation, not async network I/O
- the UI loop is naturally synchronous
- thread + channel architecture is easier to reason about

Suggested defaults:

- jobs = `min(available_parallelism, 8)`
- keep it user-configurable

That cap matters because blame can still become CPU and disk heavy on large repositories.

One implementation detail to validate early:

- each worker should open its own `git2::Repository` handle rather than sharing one mutable handle through locks

## Memory Strategy

Keep in memory:

- repo tree topology and progress metadata
- global author table for root
- file summaries for completed files
- a small LRU of scope aggregates

Avoid keeping:

- every blame hunk forever
- every per-directory author map forever
- every per-node commit set forever

If we need file-detail drill-down later, re-run blame for the selected file on demand or cache a short-lived file detail view.

## Crates

Recommended core crates:

- `clap`
  - CLI argument parsing.
- `git2`
  - repository access and blame engine.
- `ratatui`
  - main TUI rendering library.
- `crossterm`
  - terminal events and backend.
- `crossbeam-channel`
  - communication between workers and the UI thread.
- `anyhow`
  - pragmatic application-level error handling.
- `thiserror`
  - typed internal errors where useful.
- `bstr`
  - correct handling of non-UTF-8 Git paths.
- `smol_str`
  - cheap owned strings for extensions, labels, and compact identifiers.
- `lru`
  - scope aggregate cache.
- `smallvec`
  - reduce small allocation overhead in compact stats structures.

Optional crates:

- `serde` / `serde_json`
  - if we add machine-readable exports.
- `tracing` and `tracing-subscriber`
  - useful once there is enough complexity to justify structured diagnostics.
- `tui-tree-widget`
  - only if a custom tree renderer becomes annoying; I would not start with it.
- `ignore`
  - only if we later add a working-tree mode that walks filesystem state instead of a revision tree.

Crates I would avoid for v1:

- `tokio`
  - adds runtime complexity without solving a hard problem here
- `gix` as the primary blame engine
  - future option, but not the shortest path

## Proposed Module Layout

```text
src/
  main.rs
  cli.rs
  app.rs
  event.rs
  scheduler.rs
  model/
    mod.rs
    author.rs
    file_summary.rs
    filters.rs
    tree.rs
    scope_cache.rs
    file_index.rs
  git/
    mod.rs
    backend.rs
    git2_backend.rs
    revision_walk.rs
    blame_extract.rs
  tui/
    mod.rs
    layout.rs
    tree_view.rs
    author_table.rs
    files_view.rs
```

This keeps the boundaries clean:

- `git/` knows how to talk to the backend
- `model/` knows how to aggregate
- `tui/` only renders app state

## Milestones

### Milestone 1: Headless Engine

- file discovery
- parallel blame execution
- `git2` hunk extraction producing `FileSummary`
- root aggregate only
- non-interactive report output

Success criterion:

- produces stable repo-wide author rankings
- prints progress as files finish

### Milestone 2: Basic TUI

- path tree
- root progress bar / counters
- author table for root
- keyboard navigation

Success criterion:

- useful as a live viewer even before path drill-down is complete

### Milestone 3: Dynamic Path Drill-Down

- scope cache
- selected-node aggregate rebuild
- files view for selected scope
- zoom in / zoom out

Success criterion:

- moving around the tree feels responsive during an active scan

### Milestone 4: Dynamic Filtering Controls

- filter state model wired into the app
- subtree-only and file-extension filters
- include/exclude toggles for selected paths
- generation bumps for scan-affecting changes
- visible aggregate recomputation without blocking the UI

Success criterion:

- changing the active filter set feels intentional and does not corrupt in-flight results

### Milestone 5: Quality and Correctness

- rename/copy flags
- ignored revisions support if the behavior is good enough in `git2`
- better warning handling for binary or unblamable files
- fixture-based integration tests
- compatibility fallback plan if a required `git2` behavior is missing

Success criterion:

- output is trustworthy on real repos with common edge cases

## Testing Strategy

We should invest in fixture repos early.

Tests to write:

- focused tests for hunk-to-summary extraction
- integration tests on tiny temporary git repos
- rename/copy behavior tests
- ignored revision tests
- binary file skip tests
- non-UTF-8 path handling tests where platform permits

Also add a smoke test for:

- "scan a small repo with 2 workers and render a few TUI frames without panic"

And one comparison harness for correctness:

- run the same fixture against native `git blame` and compare aggregate results

## Main Risks

### 1. `git2` vs Native Git Behavior Gaps

Some blame edge cases may differ from native Git, especially around ignored revisions or copy/move semantics.

Mitigation:

- test against fixture repos and compare with native Git
- keep the backend trait clean
- add a CLI fallback backend only if real gaps matter

### 2. Commit Count Cost

Exact per-scope distinct commit counting can become expensive if done eagerly for every tree node.

Mitigation:

- cache selected scopes only
- keep root hot
- rebuild other scopes lazily

### 3. TUI Jank Under Heavy Event Load

If every completed file triggers a full resort and redraw, the UI may stutter.

Mitigation:

- batch updates inside the event loop
- redraw on a fixed tick, for example 10-20 FPS
- mark tables dirty and only resort when necessary

### 4. Repository Handle and Threading Semantics

`git2` types do not all share cleanly across threads.

Mitigation:

- keep workers isolated
- open repository handles per worker
- convert blame results into owned app data immediately

## Recommendation

Build v1 around:

- `git2` backend
- thread + channel worker pool
- compact file summaries
- root-hot + selected-scope-cached aggregation
- ratatui/crossterm TUI

That gives us a path to a working product quickly while keeping the implementation Rust-native and structurally clean.

If `git2` correctness gaps matter in practice, the backend trait gives us room to add a native-`git` compatibility backend without redesigning the rest of the system.
