# How it works

This part explains the design and points to the code that does each step.

- [The trade in one transaction](one-transaction.md): why the trade is all-or-nothing.
- [The two-party PCZT flow](pczt-flow.md): who builds, proves, signs and broadcasts, and what
  the seller checks before it signs.
- [Items as notes](items.md): how an item is minted and what makes it unique.
- [Listings and signatures](listings.md): the listings file and how it is signed.
- [Item history](history.md): tracing an item from chain data with viewing keys.
- [Ownership proofs](ownership.md): showing that you hold an item now, against a checker's
  challenge.
- [Architecture](architecture.md): the crates, how they call each other, and hosted mode.

The repository also has the protocol notes `spec/PROTOCOL.md` and the threat model
`spec/THREAT_MODEL.md`.
