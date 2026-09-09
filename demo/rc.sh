# Shell setup for the Showtime demo's cards: the user's own bashrc, then a
# staged prompt so no real user or host name ends up in a recording.
[ -f ~/.bashrc ] && . ~/.bashrc
PROMPT_COMMAND=
PS1='\[\e[1;32m\]dev@kindlyTerm\[\e[0m\]:\[\e[1;34m\]\w\[\e[0m\]\$ '
