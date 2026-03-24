# SydonOS
Reseach mulikernel build in a safe language ( Clar ).

# Why Clar?
Why clar and not other bigger programing language that guarantees memory safety? or better languages like C or Zig?

Clar is a transplater to C, it has a simple ownership model and a tri-modal safety. That means it has three modes declared at the top of the file:
1 Unsafe - the compiler just thinks you know what you do, in this mode you can just use raw C without any problem, or alternate from raw C to Clar
2 Safe relaxed - only warnings about some possible "unsafe" things and only errors to big security problems, in this mode you need to write Clar code
3 Safe strict - you need to start "proofing" the code, something like in rust

Why not Rust?

From past projects i know that to write low level Rust code you will use lots of unsafe blocks and you will start fighting with the compiler. Rust is very good at drivers and higher level things but Clar offers some features that make kernel developement much easier.

Why not Zig?
Zig sounds like a good language but my hands on a kinesis advantage2 hurt because it has so many symbols. This is the only reason i dont like zig but it was a big inspiration to Clar

Why not C?
Too much C

Is Clar mature enough?
Yes and no, Clar was not hard to make it translates something like 2:1 with C code. The only things that still need to be developed are the C output to be sure it is as optimized as possible and the ownership model is not as developed as rust. The idea with Clar ownership and borrowship model is to be as simple as possible. If something really cannot be implemented in Clar then we can just use C and develop Clar to support that, but those casses are again very uncommon because the code isn't dependent on some libs.

Do i need to learn Clar to understand the code?
No, if you know C it is enough to understand what happens and if you really want to do something with it you can write C code. Clar is simple and clean

What if you stop maintaining Clar?
I don't need to maintain Clar for the code to translate to C, i need to maintain Clar to be sure the C output is as correct as possible. Clar version could just apear monthly or on random times if there is actually a big bottleneck.

For more informations about Clar, check the official repo: ...
