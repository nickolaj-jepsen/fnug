# Fnug Demo
# Run from the project root: vhs docs/demo.tape

Require fnug

Output docs/demo.gif

Set Shell "bash"
Set FontSize 16
Set Width 1200
Set Height 700
Set Padding 12
Set Framerate 30
Set TypingSpeed 50ms
Set CursorBlink false

# Launch fnug
Hide
Type "cargo run"
Enter
Wait+Screen     /test-auto/
Show
Sleep 2s

# Make sure the first command is expanded
Down
Sleep 0.5s
Right
Sleep 0.5s
Space
Sleep 1s
Down
Sleep 0.5s
Enter
Sleep 1s
Down
Sleep 0.5s
Down
Sleep 0.5s
Screenshot docs/demo.png
Down
Sleep 0.5s


# End
Sleep 5s
