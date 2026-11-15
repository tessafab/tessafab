# Portal

A basic web front end for the engine: open a sliced plate, choose a pattern,
download the patterned G-code. Not started yet.

It will talk to the engine server (`crates/server`, JSON-RPC over stdio) or to
the engine built for the browser, and stay within what this repository builds:
one piece on one plate.
