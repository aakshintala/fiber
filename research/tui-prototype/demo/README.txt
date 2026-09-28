Fiber TUI prototype — a look and a few minutes of your time
=============================================================

WHAT THIS IS

A throwaway prototype of the terminal screen for Fiber, a coding agent
harness. It replays a recorded session — a fixed log of what happened in a
real run — so you can see and interact with the screen the way it will
eventually look. Nothing on your machine talks to a model, and nothing goes
over the network: it just reads the recorded files in this folder and draws
them to your terminal.

HOW TO RUN IT

Open a terminal, go into this folder, and run:

    ./feedback-wizard.sh

It walks you through everything one step at a time: what to try, then a
quick 1-5 rating and an optional note. Quit the prototype itself with
Ctrl+C whenever a step says to — that brings the wizard back.

WHAT YOU NEED

- A terminal window at least 118 columns wide. The wizard checks this and
  tells you if it's too narrow.
- A mouse or trackpad — several steps use scrolling, clicking and dragging.
- macOS on Apple Silicon, or Linux on x86_64 or arm64. The wizard picks the
  right binary for your machine automatically.

HOW LONG IT TAKES

About 15-20 minutes: a couple of minutes of setup, then ten short steps
(roughly a minute each) trying different parts of the screen, and a couple
of minutes of closing questions at the end.

WHAT LEAVES YOUR MACHINE

Nothing, unless you choose to send it. The only thing the wizard writes is
one plain-text file, fiber-demo-feedback.txt, in this same folder. Once
you're done, please send that file back to whoever shared this demo with
you — the wizard tells you where it saved it.
