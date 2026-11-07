# measured-boot

This crate contains models for measured boot, for common use between admin-cli and nico-api. It will optionally
derive sqlx metadata for each type if the `sqlx` feature is enabled, and will
optionally contain code to render CLI tables through prettytable, if the `cli` feature is
enabled.

## Where should I put business logic for measured-boot?

Generally any logic the server needs should go in the api crate, and any logic the CLI needs should go in the admin
crate, and this should be kept as close as possible to pure type definitions.

Currently this isn't entirely the case, as there is a lot of CLI-related business logic here. It's done out of
convenience, since Rust does not allow implementing new traits on foreign types, meaning that if the CLI wants these
types to implement ToTable, etc, it can't, unless they're defined here. The closest thing we could do is use
the [NewType pattern](https://doc.rust-lang.org/rust-by-example/generics/new_types.html), but that would be a lot of
work for now, so we try to do the next best thing by at least
making the CLI-related stuff optional and only building it when building the admin crate.

## Why not just use protobufs as the models and omit this crate entirely?

That's probably what we should eventually do. This code orignally lived in the api crate, and the admin crate was
depending on the api crate to import them... the code was moved here to make an easy transition to eliminate this
dependency. But at this point these types don't do much more than directly wrap the protobuf types, so at some point we
should just use the protobuf types directly, and move the table-generating code to the rpc crate (or even better, use
newtypes in the admin CLI to wrap the rpc types and move the table generating code there.)