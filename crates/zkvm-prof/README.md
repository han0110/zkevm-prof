# zkvm-prof

Prices one execution of a guest on a zkVM, and reads the peak heap that execution reached.

The library takes a guest ELF as bytes and an input as bytes. It knows nothing about what a guest
computes or what it is handed, so it profiles any guest built for a zkVM it supports. Fetching an
ELF, encoding an input and writing the results out are the caller's.

## What each zkVM is priced with

| zkVM | Cost | Peak heap |
| --- | --- | --- |
| OpenVM | Metered cost, which is the trace cells the app VM commits to | Read from guest memory |
| SP1 | Gas without the division that rounds it, which keeps the weighted sum exact | Read on `x86_64` Linux only |
| ZisK | The weighted sum the emulator accumulates | Read from guest memory |

SP1 keeps guest memory behind its executor except on the target that executor compiles a native JIT
for. Elsewhere the cost is still priced and the peak heap comes back absent.

## Use

```rust
use zkvm_prof::{profiler, zkVMKind};

let elf = std::fs::read("guest.elf")?;
let input = std::fs::read("input.bin")?;

let profiler = profiler(zkVMKind::Zisk, &elf)?;
let execution = profiler.profile(&input)?;
println!("{}", execution.cost.values().sum::<u64>());
println!("{:?}", execution.peak_heap_bytes);
```

A profiler is built once per ELF, so whatever it derives from that ELF is paid for once and reused
across every input. It is `Sync`, so one profiler serves many threads. A profiled execution holds
the whole guest memory, so a machine with many cores and little memory has to cap its thread count.

Build one profiler per process. The OpenVM backend leaks the SDK it builds, so each OpenVM profiler
a process constructs holds its memory until the process ends.

`profile` is the whole of the interface. A guest that breaks a zkVM invariant aborts the emulator by
panicking rather than by returning, and every backend catches that and reports it as the error it is,
so one bad input costs a caller that input and nothing more.

## Cost kinds

`Execution::cost` is an open map of whatever kinds the zkVM charges for. Those kinds partition the
execution, so they sum to what the whole of it cost and no key holds that whole. `composition(zkvm)`
names them in the order a chart stacks them, and each one carries the note a report prints under that
chart.

A backend prices an execution one way and splits it another, so it checks the two against each other
before returning. The sum is therefore the zkVM's own figure rather than a second reading of it.

## Build

OpenVM execution goes through rvr, which needs LLVM clang 19 or newer and a matching lld. Ubuntu
ships a clang older than rvr accepts and no lld at all, so both have to be installed first.
