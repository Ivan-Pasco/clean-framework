# The `mobile` world — a first draft of `clean-mobile-surface`

**Status: SUPERSEDED as a draft, retained as the reasoning record.** The contract now
exists at [`hosts/clean-mobile-surface/host.wit`](../hosts/clean-mobile-surface/host.wit).
Read that file for what the contract *is*; read this one for why each shape was chosen
and what was rejected. Where the two disagree, the `.wit` wins.

Date: 2026-08-26. Shipped 2026-08-27, with three changes forced by contact with the code:

1. **`include` was abandoned — because of the transport, not because of WIT.** §2's
   composition rule survives; its mechanism does not. `hostwit::resolve_contract` fetches
   exactly one URL per host and yields one string, pinned by SHA-256 in `.cln/lock.toml`
   (BVER-03); `declares_world` then checks it *textually* ("a courier, not a WIT
   compiler"). Only mobile's own file ever reaches the compiler, so an `include` of
   clean-browser resolves nothing.

   WIT itself would allow it, and an earlier reading of this that blamed the shared
   `package clean:host@0.1.0` name was **wrong**: two files sharing a package name
   resolve together fine, and a world in one may include a world in the other. (The
   symptom that suggested otherwise was the `//` banner parsing as a doc comment on
   `package`.) Inlining `world browser` into mobile's file would make `include` legal,
   but inherits only the `export` lines while still copying every interface body — the
   appearance of reuse without the substance.

   So composition is by duplication, guarded by a drift test rather than a resolver.
   Costs nothing today: `world browser` is empty. Genuine reuse would need a multi-file
   contract transport — a framework-wide change to fetching, hashing and pinning, wanting
   its own spec amendment.
2. **The blocker dissolved rather than being waited out.** `clean-browser` now ships an
   empty `world browser { }` at [`hosts/clean-browser/host.wit`](../hosts/clean-browser/host.wit),
   which open question 2 had already identified as correct under HCV-06.
3. **`option<position>` replaced the bare `position`** in `position-fixed`, so a fix that
   never arrives is expressible without inventing a sentinel coordinate.

The three framework edits the memory predicted are done: `wasm32-mobile` in the `WORLDS`
table, its `mobile` mapping, and `clean-mobile-surface` in `OFFICIAL_HOSTS`. Platform 07
§7.2 was amended first, as CONF-02 requires — the arity assertion in `manifest.rs` is what
made that non-optional.

---

## 1. What this host is

`clean-mobile-surface` runs a Clean guest as wasm **in the WebView's engine**, inside a
Tauri Rust shell on iOS and Android. The name says where the guest lives: on the surface,
in the page. A future peer host could run the guest in an embedded wasmtime instead —
`clean-mobile-runtime` is the natural pair, though that name is not committed.

Both hosts would target `wasm32-mobile` and implement the same `mobile` world. That is
the whole point of the naming split: switching seats should be a `clean.toml` edit, not
a rewrite. Everything in this draft is written to keep that true.

## 2. The composition rule

**`clean-mobile-surface` provides the browser contract plus a device package.** It does
not restate the browser surface, and it does not declare one monolithic `mobile` world
that forks from browser and drifts.

This mirrors what `clean-server` already does: it declares the HTTP surface it owns and
explicitly notes that `wasi:cli/*` and the rest of the WASI stack come from
clean-host-core and are "not restated here — host.wit declares the `clean:*` surface this
host owns."

The consequence worth having: a guest that touches no device API is portable between
browser and mobile **unchanged**. One that opens the camera declares that import
explicitly, and the declaration is what makes it non-portable — visibly, in its own
source, rather than by surprise at load time.

## 3. The vocabulary rule

Device interfaces are phrased in **device** terms, never browser terms.

This is the single constraint that keeps the wasmtime seat reachable. Any DOM handle,
JS value, or browser-lifecycle concept that leaks into these signatures becomes precisely
the thing a native host cannot honour, and the door this whole naming scheme exists to
hold open closes quietly.

Concretely: `capture-photo`, not `get-user-media`. `position`, not `GeolocationPosition`.
A permission is a host concept, not a `navigator.permissions` query.

## 4. Asynchrony: handles and callbacks, not futures

> **Correction (2026-08-27).** Everything in this section holds, but the `world`
> block in §5 is **host-authored only**. `HostInterface::to_wit`
> (`crates/framework-core/src/hostbridge.rs`) emits exactly one bare `interface`
> of guest→host `func`s — no `world`, no `import`, no `record`, no `enum`, no
> `include`. The grammar has no production for any of them.
>
> So a `frame.device` *library* can declare only the outbound half:
> `capturePhoto(quality: integer, callback: handler) returns integer`, where
> `handler` crosses as the `u32` index the compiler assigns. The inbound half —
> every `import` line in §5's world block — can live only in this hand-written
> `host.wit`, never in a `host_bridge.cln`.
>
> That is workable, since the host file is hand-authored anyway. It does mean the
> two halves of one device API are declared in two places by two owners, and
> nothing checks they agree. Worth knowing before writing the library.

The shipped contracts do not use `future<>` or `stream<>`. `clean-server` handles
asynchrony with a handle plus a guest-side id: SSE's `start: func() -> result<u64, ...>`
returns a stream handle, and `routing.register` takes a `handler-id: u32` the guest
supplies. The host calls back into the guest against that id.

Device APIs are unavoidably async — the camera takes as long as the user takes. This
draft follows the established idiom rather than introducing a second one:

- the guest calls a `request-*` function, supplying a `request-id: u32` it chose
- the host returns immediately with `result<_, error>` — did the *request* start
- when the operation completes, the host invokes an exported callback with that id

If WIT-native `future<>` is later adopted across all Clean hosts, these convert
mechanically. Adopting it *here first* would make mobile the odd host out.

## 5. The draft

```wit
// clean-mobile-surface — the host contract. DRAFT, nothing implemented.
//
// HCV-02: this file would be the authoritative declaration of what this host
// provides. HCV-06: CI must verify every declared interface is really registered
// by the shell, and that nothing registered is undeclared.
//
// SCOPE NOTE: interfaces below are grouped by confidence. Only the ones backed by
// an official Tauri plugin today are candidates for a first real contract; the
// rest are shown to fix vocabulary, and MUST be omitted from any shipped file
// until implemented.

package clean:host@0.1.0;

/// Permission state for a device capability.
///
/// Declared once and shared, because every device interface needs it and three
/// slightly different spellings of "denied" would be worse than one.
interface permissions {
    enum state {
        /// Never asked. The guest may prompt.
        undetermined,
        /// The user allowed it.
        granted,
        /// The user refused. Asking again is usually a bad experience and on
        /// iOS is frequently a no-op — the platform will not re-prompt.
        denied,
        /// Policy forbids it (parental controls, MDM). Not the user's choice
        /// and not recoverable by asking.
        restricted,
    }

    enum capability {
        camera,
        location,
        notifications,
        biometrics,
    }

    /// Current state. Never prompts.
    query: func(c: capability) -> state;

    /// Request access, prompting if the state is `undetermined`.
    /// Async: the user is a slow dependency. Result arrives via
    /// `permission-result` on the guest.
    request: func(c: capability, request-id: u32) -> result<_, string>;
}

/// Photo capture (Tauri: camera plugin).
interface camera {
    enum facing { front, rear }

    record capture-options {
        which: facing,
        /// JPEG quality, 1-100. The host clamps rather than erroring, because
        /// a bad number here should not fail a user-initiated capture.
        quality: u8,
    }

    enum capture-error {
        /// `permissions.query(camera)` was not `granted`.
        not-permitted,
        /// No usable camera. A tablet with the front sensor covered, say.
        unavailable,
        /// The user backed out of the capture UI. Not an error condition to
        /// report to the user — they know.
        cancelled,
        /// Everything else, with a host-supplied description.
        failed,
    }

    /// Begin a capture. Returns once the capture UI is up, not once the photo
    /// exists. The image arrives via `photo-captured`.
    request-capture: func(opts: capture-options, request-id: u32)
        -> result<_, capture-error>;
}

/// Device position (Tauri: geolocation plugin).
interface location {
    record position {
        latitude: f64,
        longitude: f64,
        /// Metres above the WGS-84 ellipsoid. Absent when the fix is 2-D,
        /// which is common indoors — hence option rather than a sentinel.
        altitude: option<f64>,
        /// Horizontal accuracy radius in metres, 68% confidence.
        accuracy: f64,
        /// Degrees from true north. Absent when stationary: heading is
        /// derived from movement and is meaningless without it.
        heading: option<f64>,
        /// Metres per second. Absent for the same reason.
        speed: option<f64>,
        /// Milliseconds since the Unix epoch, host clock.
        timestamp: u64,
    }

    enum location-error {
        not-permitted,
        /// Location services off device-wide, or no signal.
        unavailable,
        timed-out,
        failed,
    }

    /// One fix. Arrives via `position-fixed`.
    request-position: func(request-id: u32) -> result<_, location-error>;

    /// Continuous updates until `stop-watching`. Each update arrives via
    /// `position-fixed` carrying this same id — so the guest distinguishes a
    /// one-shot from a stream by what it asked for, not by the callback.
    start-watching: func(request-id: u32, min-interval-ms: u32)
        -> result<_, location-error>;

    stop-watching: func(request-id: u32);
}

/// Local notifications (Tauri: notification plugin).
///
/// Remote push (APNs/FCM) is deliberately NOT here. It needs server-side
/// credentials and a token lifecycle, which is a different contract with a
/// different trust boundary — folding it in would put key management behind a
/// function that looks like a UI call.
interface notifications {
    record notification {
        title: string,
        body: string,
        /// Guest-chosen id, so it can be cancelled or replaced later.
        id: u32,
    }

    show: func(n: notification) -> result<_, string>;
    cancel: func(id: u32);
}

/// Biometric authentication (Tauri: biometric plugin).
///
/// Deliberately narrow: this answers "did the device confirm the user is
/// present" and nothing else. It does not return biometric data, and it must
/// never look like it could.
interface biometrics {
    enum kind {
        /// Face ID, or Android face unlock.
        face,
        /// Touch ID, or Android fingerprint.
        fingerprint,
        none,
    }

    /// What this device supports. `none` means unenrolled or absent hardware —
    /// the guest should fall back rather than prompt.
    available: func() -> kind;

    /// Prompt for confirmation. `reason` is shown to the user by the OS and is
    /// mandatory on iOS. Result arrives via `biometric-result`.
    request-verify: func(reason: string, request-id: u32) -> result<_, string>;
}

/// Application lifecycle.
///
/// Phrased as device/app states, not `visibilitychange`. A native host has the
/// same four states; a browser lifecycle event would not survive the move.
interface lifecycle {
    enum app-state {
        /// Visible and receiving input.
        active,
        /// Visible but not focused — iOS control centre pulled down, split
        /// screen without focus.
        inactive,
        /// Not visible. Execution may be suspended at any moment.
        background,
    }

    current: func() -> app-state;
}

/// The world.
///
/// `include browser` is the composition rule from §2: the UI surface is
/// inherited from clean-browser, not restated. That include is why this world
/// cannot be written until clean-browser's contract exists.
// HOST-AUTHORED ONLY. None of this block can be synthesized from a
// `host_bridge.cln` — see the correction in §4. A library declares the
// outbound `export` half via `host function`; the `import` callbacks below
// exist only here.
world mobile {
    // include clean:host/browser;   // <- blocked: no browser contract yet

    export permissions;
    export camera;
    export location;
    export notifications;
    export biometrics;
    export lifecycle;

    /// Callbacks. The host calls these against the request-id the guest chose.
    import permission-result: func(request-id: u32, granted: bool);
    import photo-captured: func(request-id: u32, image: list<u8>);
    import position-fixed: func(request-id: u32, pos: location.position);
    import biometric-result: func(request-id: u32, verified: bool);

    /// Fired on every transition, not polled. A guest that needs to flush
    /// state before suspension cannot afford to find out by asking later.
    import app-state-changed: func(to: lifecycle.app-state);
}
```

## 6. What is deliberately absent

**Bluetooth.** No official Tauri plugin. Community crates exist and are unvetted. Under
HCV-06 it cannot be declared until something real backs it.

**Remote push.** Reasoning in the notifications comment above — different trust boundary,
different contract.

**Filesystem.** Tauri has an official `fs` plugin, but the interaction with WASI's own
filesystem view needs deciding first. Two overlapping ways to open a file is worse than
one late one.

**NFC, haptics, clipboard, share.** All have official plugins and would be
straightforward additions. Omitted here only to keep the first draft small enough to
argue with.

## 7. Open questions

1. **The peer host's name.** `clean-mobile-runtime` pairs with `-surface` on the
   where-does-the-guest-live axis, but it is not committed. Worth settling before the
   first host ships, not after.

2. **~~Whether `include` is the right composition mechanism.~~ RESOLVED: no — one file
   per contract, so duplication plus a drift test. See the status note above.** WIT `include` pulls a world
   into another. Whether that or explicit re-export better expresses "browser plus
   devices" depends on how `clean-browser` declares itself. Decided since: that contract
   ships as an **empty `world browser { }`**, because HCV-06 forbids declaring what no
   host registers and no browser host exists. An include of an empty world composes just
   as well, and every later browser addition is inherited for free.

2a. **~~The two halves are declared by two owners.~~ RESOLVED 2026-08-27: checked, coarsely.**
   A device API's outbound functions live in `frame.device`'s `host_bridge.cln` and its
   callbacks in this `host.wit`, and the compiler's World Import Check does *not* cover
   the gap: the guest imports nothing wrong, so it is satisfied, and the symptom is a
   callback that never arrives on a device.

   `crates/framework-core/src/callback.rs` now checks it during the build, beside LIB012
   where the interface is already parsed. A library declaring a `handler` parameter is
   asking to be called back; if the target world declares no callback imports at all, the
   build fails naming the library, the function, and the host contract to edit.

   **The check is deliberately coarse.** It asks "can this host call back at all", never
   "does the *right* callback exist". Matching `capturePhoto` to `photo-captured` would
   need a naming convention the spec never stated and a library author could not discover.
   The narrower check becomes possible the day a library can name the callback it expects
   — `callback.rs` is where it would go. A coarse check that never cries wolf is worth
   more than a precise one built on an invented rule.

3. **Callback delivery on the wasmtime seat.** In the WebView the callbacks are a JS
   bridge hop. In an embedded wasmtime they are direct component calls. The *signatures*
   are identical, which is the point — but confirming the semantics match (ordering,
   reentrancy, what happens if the guest calls back into the host from a callback) is
   work that has not been done.

4. **`list<u8>` for image data.** A photo is megabytes, and copying it across the
   boundary may be the wrong default. `clean-server` faced the same question for request
   bodies; whatever it settled on should probably apply here too.
