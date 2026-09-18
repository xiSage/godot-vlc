# Building on the libvlc binding

This is about the shape of the API this extension exposes and the claims its
documentation makes. It is not about running it, building it or the test harness:
those have their own documentation.

## Bind, do not redesign

A binding whose names differ from the library's turns every lookup into a
translation. A one-to-one wrapper is therefore the libvlc function name with the
library prefix dropped -- `stop_async`, `unselect_track_type`, `tracklist_at` --
including the spellings libvlc chose and a reviewer might have been tempted to
correct.

The shape matters as much as the name. When libvlc answers a question with one
call that returns a status and fills in four out-parameters, the binding answers
it with one getter that returns all five; splitting it into five getters invents
an API libvlc does not have and makes the caller put it back together.

Names of our own belong to things that are not a libvlc function -- a resource
loader, a field of a C struct, a convenience -- and have to read like it.
`get_texture` and `get_buffering_percent` are ours; nothing should mistake them
for a libvlc entry point.

Before adding anything, answer three questions: does libvlc have this function,
what is it called, and what is its shape?

## The pinned runtime is the authority, not the headers

Every claim that reaches the documentation has been read out of the source at the
revision `build/vlc/vlc.lock` pins, and measured against the runtime that ships.
Where a header comment and the implementation disagree, the documentation follows
the implementation and says so. Two examples, both documented that way now:
`play()` and `stop_async()` promise `-1 on error` and return `VLC_EGENERIC`, and
`libvlc_media_player_get_abloop` describes an out-parameter contract its
implementation does not honour.

This applies with more force to negatives, because a design rests on them.
"libvlc cannot do this", "there is no event for that", "this needs a seekable
input" are inputs to a decision, so they need a quote or a measurement behind
them. Several statements that looked obvious did not survive the probe: a loop
can be set before playback after all, a B point past the end of the media does
not fail, and the per-media `:input-repeat` option that looked like the other way
to loop has no reader anywhere in the tree.

## Turn C's undefined into defined

The C API is willing to hand back something that means nothing: out-parameters
written only when a status says they apply, a union member no sender ever fills,
a value that is uninitialised stack memory. Making those cases defined is the
binding's job -- read what the status covers and nothing else, never read the
member that is not there, and say "not applicable" with a sentinel instead of
passing the garbage on. Inventing a value libvlc did not give is as wrong as
dropping one it did.

## Stay thin: do not keep state libvlc already keeps

A second copy of libvlc's state drifts from the first, so a binding that caches
it is a binding that eventually lies. Prefer exposing the lifetime as it is and
documenting it: a setting that belongs to the input dies with the input, and the
caller is told to set it again. That is why there is no `loop: bool` -- it would
either cache an intention and re-apply it after every play, or be a switch that
quietly stops working after a stop.

Where the binding does keep state, the reason has to be specific and the source
has to be the one the signals already use. The buffering percentage is cached so
that a script connecting late can still read it, not so that the binding has an
opinion of its own.

## Callbacks record, the main thread reports

libvlc calls back on its own threads, holding its own locks, and from there no
Godot object and no `libvlc_media_player_*` call is safe. So a callback does one
thing: it records what it received. The main thread turns those records into
signals once a frame, in arrival order, because that order is part of what the
events mean. Events that arrive in bursts are compared and reported only when
they move.

## Do not lie to the caller

Return values and failures are passed through as libvlc gives them rather than
improved: `0` and `-1` stay `0` and `-1`, and a failure that arrives
asynchronously stays visible instead of being smoothed into silence. What libvlc
does not offer is not invented either: an event with no payload produces a signal
with no arguments, and where there is no event at all the documentation says
there is none instead of implying a signal that will never arrive.

## The in-editor documentation is the product

The README covers platforms and rendering paths; the API is explained where it is
used, in the doc comments, because that is the only documentation a user of this
addon reads. Every signal and method therefore says what it guarantees and what
it does not -- silent failures, imprecision, absent events, per-input lifetimes,
platform differences -- with the measured numbers where numbers exist: a media
that cannot be opened is reported about 140 ms later, a loop's B point lands
0-100 ms late and cannot be trusted below about 400 ms.

Being ugly is allowed. Being misread is not.
