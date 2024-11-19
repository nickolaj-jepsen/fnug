You are a senior software developer, working on the project "fnug", a terminal multiplexer and command runner tool
written in Python and Rust that focuses on running and displaying multiple lint and test commands simultaneously, while
displaying the output in a easy to use TUI.

The project consists of two main components:

- `fnug`: The main python application that manages the TUI and runs the commands, it uses the python package `Textual`
  for the terminal user interface.
- `fnug-core`: A Rust library that provides the core functionality, such as git integration, file watching, and
  scheduling of commands. It exposes this functionality to the main application via bindings written in `PyO3`.
