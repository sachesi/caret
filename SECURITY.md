# Security

Please report a vulnerability privately rather than in a public issue: through
[a private advisory](https://github.com/sachesi/caret/security/advisories/new) on GitHub,
or by mail to sachesi <xsachesi@pm.me>. Say what you found, how to reproduce it and which
version you ran; a fix is worked out with you before anything is published.

Only the latest release gets fixes.

## What counts

A terminal shows whatever a program writes, and the text may come from anyone: a file
being printed, a log, a server at the other end of `ssh`. The parts where a mistake
matters most:

- Escape sequences that make the terminal act for the program: writing to the clipboard
  (reading it is refused), setting the title, answering queries. A way for text on screen
  to read the clipboard, to type into the program as if the user had, or to reach the
  system outside the terminal is a vulnerability.
- Pasting: text in bracketed paste loses its own escape sequences, so a paste cannot end
  the bracket early and run what follows.
- Links: only what the user clicks with Ctrl held, or opens from the menu, is opened.
- The renderer: glyphs, the atlas and GL resources, where a sequence of output could
  crash the terminal or read memory it should not.
