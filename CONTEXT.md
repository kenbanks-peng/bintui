# Executable Registry

A personal macOS tool for declaring which local executable targets are published under stable command names.

## Language

**Registration**:
The desired binding of one command name to one target, including whether that binding is enabled.
_Avoid_: Entry, command, executable

**Command Name**:
The stable name through which a user invokes a registered target.
_Avoid_: Command, link name

**Target**:
The executable filesystem object to which a registration refers.
_Avoid_: Command, source command

**Managed Link**:
A published symbolic link that the executable registry owns as generated state for an enabled registration.
_Avoid_: Command, executable
