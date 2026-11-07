# Scheduler Architecture

Internal job scheduling framework for Flow. Handles time-based, startup, and
event-driven jobs with configurable overlap policies.

---

## Package Structure

```text
internal/scheduler/
  scheduler.go          — Scheduler: Schedule, Start, Stop
  entry.go              — entry (job+trigger+policy+channels), workItem
  relay.go              — relay: g1 (intake) + g2 (dispatch) pipeline
  dispatcher.go         — dispatcher interface + Skip/Queue/QueueAll/Replace
  worker.go             — worker: executes jobs sequentially from workCh
  types/
    job.go              — Job interface: Name(), Run(ctx, Event)
    event.go            — EventType, Event{Type, Payload}
    policy.go           — Policy: Skip, Queue, QueueAll, Replace
    trigger.go          — Trigger interface + IntervalTrigger, CronTrigger,
                          OnceTrigger, EventTrigger
  jobs/
    inventorysync/      — InventorySync job implementation
    leakdetection/      — LeakDetection job implementation
```

---

## Component Overview

```mermaid
graph TB
    subgraph Triggers["Trigger (types/trigger.go)"]
        IT["IntervalTrigger"]
        CT["CronTrigger"]
        OT["OnceTrigger"]
        ET["EventTrigger"]
    end

    subgraph Scheduler["Scheduler (scheduler.go)"]
        S["Scheduler\nSchedule / Start / Stop"]
    end

    subgraph Entry["Per-Entry (entry.go)"]
        E["entry\njob + trigger + policy\neventCh · workCh"]
    end

    subgraph RelayBox["Relay (relay.go)"]
        G1["g1 intake\nreads eventCh\nwrites queue\npings notifyCh"]
        Q["queue\n[]Event\n(max 2048)"]
        G2["g2 dispatch\nreads notifyCh\ndelegates to dispatcher"]
        G1 -- "append" --> Q
        G1 -- "ping / close" --> G2
        Q -- "dequeue" --> G2
    end

    subgraph DispatcherBox["Dispatcher (dispatcher.go)"]
        D["Skip / Queue\nQueueAll / Replace"]
    end

    subgraph WorkerBox["Worker (worker.go)"]
        W["worker\nsequential\njob.Run(ctx, ev)"]
    end

    subgraph Jobs["Jobs"]
        IS["InventorySyncJob"]
        LD["LeakDetectionJob"]
    end

    IT & CT & OT & ET --> S
    S --> E
    E -- "eventCh" --> G1
    G2 --> D
    D -- "workCh" --> W
    W --> IS
    W --> LD
```

---

## Per-Entry Pipeline

Each registered job runs in its own isolated pipeline of three goroutines:

```mermaid
flowchart LR
    TR["Trigger\n.Emit()"]
    EC["eventCh\ncap=1"]
    G1["g1 intake"]
    Q["queue\nmax 2048"]
    NC["notifyCh\ncap=1"]
    G2["g2 dispatch"]
    WC["workCh\nunbuffered"]
    W["worker"]

    TR -->|"Event"| EC
    EC -->|"read"| G1
    G1 -->|"append"| Q
    G1 -->|"ping / close"| NC
    NC -->|"wake"| G2
    Q -->|"dequeue"| G2
    G2 -->|"workItem{ctx,ev}"| WC
    WC -->|"read"| W
```

**g1 — intake:** Reads `eventCh`, buffers into `queue` (drops oldest on
overflow), and non-blocking pings `notifyCh`. On exit, closes `notifyCh` to
signal g2 that no more events will ever arrive.

**g2 — dispatch:** Wakes on `notifyCh`, delegates dequeue and send logic to
the per-policy dispatcher. Exits on `notifyCh` close, `forceCh` close, or
`ctx.Done()`.

**worker:** Reads `workItem` values from `workCh` sequentially, calls
`job.Run(ctx, ev)`. Exits when `workCh` is closed by the relay.

---

## Dispatcher Behaviours

```mermaid
flowchart TD
    N(["notifyCh ping"])
    DQ["Dequeue from queue"]
    N --> DQ

    DQ --> SK{"Skip"}
    SK -->|"worker free"| SEND1["send workItem\nregister cancelCurrent"]
    SK -->|"worker busy"| DROP1["drop event"]

    DQ --> QU{"Queue"}
    QU -->|"worker free"| SEND2["send workItem\nregister cancelCurrent"]
    QU -->|"worker busy\nno newer event"| PUT["put event back\nwait for next ping"]
    QU -->|"worker busy\nnewer event exists"| DROP2["drop event"]

    DQ --> QA{"QueueAll"}
    QA --> SNAP["snapshot entire queue\n(single lock)"]
    SNAP --> FIFO["blocking send\nFIFO order"]

    DQ --> RP{"Replace"}
    RP --> CANCEL["cancel running job"]
    CANCEL --> LATEST["take latest event\ndrop all others"]
    LATEST --> SEND3["blocking send\nregister cancelCurrent"]
```

All dispatchers embed `dispatchBase` which holds `cancelCurrent`
(`context.CancelFunc`), allowing `forceStop` to abort the in-flight job
regardless of policy. `cancelCurrent` is registered only after a successful
send to `workCh`.

---

## Lifecycle

`Scheduler` is a **single-use object**. The expected call order is:

1. `Schedule(...)` — register jobs (one or more calls)
2. `Start(ctx)` — launch goroutines; returns an error if called more than once
3. `Stop(force)` — shut down and wait; returns an error if called more than once

Calling `Start` after `Stop` is rejected with an error. Reuse is not supported
because internal channels (`eventCh`, `workCh`, `notifyCh`) are closed during
shutdown and cannot be safely re-opened. Create a new `Scheduler` instance
instead.

---

## Shutdown Sequences

### Graceful — `Stop(force=false)`

```mermaid
sequenceDiagram
    participant SC as Scheduler
    participant G1 as g1 intake
    participant G2 as g2 dispatch
    participant W  as worker

    SC->>SC: runCancel()
    G1->>G1: ctx.Done() → drain remaining eventCh items into relay queue
    Note over G1: enqueue pending events, ping notifyCh as needed
    G1->>G2: close(notifyCh)
    Note over G2: Skip/Queue/Replace: return immediately
    Note over G2: QueueAll: flush remaining events first
    G2->>W: close(workCh)
    W->>W: range workCh exits
    SC->>SC: wg.Wait() returns
```

### Force — `Stop(force=true)`

```mermaid
sequenceDiagram
    participant SC as Scheduler
    participant RL as relay
    participant G2 as g2 dispatch
    participant W  as worker

    SC->>RL: forceStop()
    RL->>RL: clear queue
    RL->>RL: dispatcher.cancel() — abort running job
    RL->>G2: close(forceCh)
    G2->>G2: exit immediately (no drain)
    SC->>SC: runCancel()
    RL->>W: close(workCh)
    W->>W: range workCh exits
    SC->>SC: wg.Wait() returns
```

---

## Trigger Types

| Trigger | Fires | Exhausted when |
|---------|-------|----------------|
| `IntervalTrigger` | Every fixed duration | ctx cancelled |
| `CronTrigger` | On robfig/cron v1 6-field schedule | ctx cancelled |
| `OnceTrigger` | Exactly once, immediately | After first event sent |
| `EventTrigger` | On each event from an external `<-chan Event` | Source channel closed or ctx cancelled |

---

## Overlap Policy Summary

| Policy | Worker busy behaviour | Event ordering | Use when |
|--------|-----------------------|----------------|----------|
| `Skip` | Drop incoming event | N/A | Job is idempotent; only one concurrent run matters |
| `Queue` | Keep latest, discard rest | Latest only | Polling jobs: next run reads current state anyway |
| `QueueAll` | Buffer all, process FIFO | Strict FIFO | Each event carries unique data; best-effort — oldest events dropped on queue overflow, queue skipped on force-stop |
| `Replace` | Cancel current, run latest | Latest only | Only the most recent trigger is meaningful |

---

## Job Interface

```go
type Job interface {
    Name() string
    Run(ctx context.Context, ev Event) error
}
```

The `Event` is passed directly to `Run` — no context value extraction needed.
For time-based triggers (`IntervalTrigger`, `CronTrigger`, `OnceTrigger`),
`ev` is a zero-value `Event{}`. For `EventTrigger`, `ev` carries the
`Type` and `Payload` from the source channel.

---

## Key Design Decisions

- **Isolated pipeline per entry** — no shared state between jobs; one misbehaving
  trigger cannot block others.
- **g1/g2 split** — g1 owns the queue under a mutex; g2 reads from it. Separating
  intake from dispatch keeps the relay lock out of the blocking send to `workCh`.
- **notifyCh closure as termination signal** — g1 closing `notifyCh` is the
  natural "no more events" signal, eliminating a separate `intakeDone` channel.
- **forceCh for immediate exit** — a dedicated closed channel lets all dispatchers
  (including QueueAll's drain loop) exit without waiting for ctx cancellation.
- **cancelCurrent registered after send** — ensures `forceStop` always targets
  an actual running job, never a context that was never delivered.
- **Event passed directly to Run** — avoids hidden coupling through context values;
  the compiler enforces the contract.
