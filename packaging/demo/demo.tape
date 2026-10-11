# Records docs/demo.gif with https://github.com/charmbracelet/vhs:
#   vhs packaging/demo/demo.tape
# Uses a throwaway config folder and the SQLite file made by
# packaging/demo/seed.sql, so it shows the same thing on any machine.

Output docs/demo.gif
Set Shell "bash"
Set FontSize 15
Set Width 1400
Set Height 820
Set Theme "Builtin Dark"
Set TypingSpeed 45ms

Hide
Type "rm -rf /tmp/podelis-demo && mkdir -p /tmp/podelis-demo && sqlite3 /tmp/podelis-demo/shop.db < packaging/demo/seed.sql" Enter
Type "export PODELIS_CONFIG_DIR=/tmp/podelis-demo/config && clear" Enter
Show

Type "podelis /tmp/podelis-demo/shop.db"
Sleep 300ms
Enter
Sleep 1.5s

# Write, run (Ctrl+R), browse the result and open a row vertically.
Type "select c.name, sum(o.total) as spent from customers c join orders o on o.customer_id = c.id group by c.name order by spent desc"
Sleep 500ms
Ctrl+R
Sleep 2s
# Esc first: Tab right after a word would complete it.
Escape
Sleep 200ms
Tab
Sleep 500ms
Down 2
Sleep 400ms
Type "x"
Sleep 2s
Escape
Sleep 500ms

# A risky write asks first; `t` runs it in a transaction to review.
Tab 2
Sleep 300ms
Ctrl+A
Type "update orders set total = total * 1.1"
Ctrl+R
Sleep 1s
Type "t"
Sleep 2s
Type "r"
Sleep 1.5s

# Command palette: Ctrl+K, then / searches every command by name.
Ctrl+K
Sleep 600ms
Type "/format"
Sleep 1.2s
Enter
Sleep 2s
