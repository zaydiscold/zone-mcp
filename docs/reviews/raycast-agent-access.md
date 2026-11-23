# Raycast and the Agents switch

Raycast's manual search and its AI tools used the same CLI reads. Turning off agent access
stopped the MCP server from returning library data, but the extension's AI reads did not go
through that server. The affected surfaces included tool results, search mentions, the item
resource provider, and note previews in a write confirmation.

The extension now checks the local library's Agents setting before a Raycast AI read and again
before returning the result. A changed setting discards the result. A missing or unreadable
setting command fails the request. An older library with no stored preference retains Zone's
default of allowing access. Writes use the existing MCP permission check. A person can still
search their library from the manual Raycast commands.

## Reproduce the regression test

Use the synthetic library setup under **Testing** in the extension README. Run
`npm test` in `raycast-extension` with `ZONE_TEST_BIN` and `ZONE_TEST_HOME` set.
The test named `Raycast AI respects the switch while manual reads remain available` checks
an allowed read, an off-state refusal, a successful manual read, and a refusal when the
switch changes during a read. It restores access in the temporary test library.
