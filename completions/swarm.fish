# Print an optspec for argparse to handle cmd's options that are independent of any subcommand.
function __fish_swarm_global_optspecs
    string join \n h/help V/version
end

function __fish_swarm_needs_command
    # Figure out if the current invocation already has a command.
    set -l cmd (commandline -opc)
    set -e cmd[1]
    argparse -s (__fish_swarm_global_optspecs) -- $cmd 2>/dev/null
    or return
    if set -q argv[1]
        # Also print the command, so this can be used to figure out what it is.
        echo $argv[1]
        return 1
    end
    return 0
end

function __fish_swarm_using_subcommand
    set -l cmd (__fish_swarm_needs_command)
    test -z "$cmd"
    and return 1
    contains -- $cmd[1] $argv
end

complete -c swarm -n "__fish_swarm_needs_command" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_needs_command" -s V -l version -d 'Print version'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "init" -d 'Create a repo key and publish the first manifest'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "link" -d 'Print the link for a repo'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "status" -d 'Show repos, sequence numbers and conflicts'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "seed" -d 'Sync a repo and keep serving it'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "unseed" -d 'Stop serving a repo but keep its store'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "gc" -d 'Repack a repo\'s store and drop unreachable objects, when idle (ST-7)'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "fork" -d 'Create an independent repo from another one'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "export" -d 'Write a repo and its signed state to a bundle file for offline transfer'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "mirror" -d 'Publish a repo for static mirrors (plain file hosting)'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "import" -d 'Read a bundle file, making its repo available offline'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "maintainers" -d 'Add or remove maintainers, who may publish without the root key'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "accept" -d 'Settle a ref conflict here by taking one signer\'s value'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "key" -d 'Key management'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "completions" -d 'Print a shell completion script (bash, zsh, fish, elvish, powershell)'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "daemon" -d 'Run, start or stop the daemon'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c swarm -n "__fish_swarm_using_subcommand init" -l object-format -d 'Object format for a newly created git repo' -r
complete -c swarm -n "__fish_swarm_using_subcommand init" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand init" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand link" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand link" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand status" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand status" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand seed" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand seed" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand unseed" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand unseed" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand gc" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand gc" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand fork" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand fork" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand export" -s o -l output -d 'Bundle file to write' -r -F
complete -c swarm -n "__fish_swarm_using_subcommand export" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand export" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand mirror; and not __fish_seen_subcommand_from export help" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand mirror; and not __fish_seen_subcommand_from export help" -f -a "export" -d 'Write DIR/<repo-id>/repo.swarmbundle; serve DIR with any web server'
complete -c swarm -n "__fish_swarm_using_subcommand mirror; and not __fish_seen_subcommand_from export help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c swarm -n "__fish_swarm_using_subcommand mirror; and __fish_seen_subcommand_from export" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand mirror; and __fish_seen_subcommand_from help" -f -a "export" -d 'Write DIR/<repo-id>/repo.swarmbundle; serve DIR with any web server'
complete -c swarm -n "__fish_swarm_using_subcommand mirror; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c swarm -n "__fish_swarm_using_subcommand import" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand import" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand maintainers; and not __fish_seen_subcommand_from add remove list help" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand maintainers; and not __fish_seen_subcommand_from add remove list help" -f -a "add" -d 'Authorize a key (root key only), optionally limited to ref globs'
complete -c swarm -n "__fish_swarm_using_subcommand maintainers; and not __fish_seen_subcommand_from add remove list help" -f -a "remove" -d 'Remove a maintainer (root key only)'
complete -c swarm -n "__fish_swarm_using_subcommand maintainers; and not __fish_seen_subcommand_from add remove list help" -f -a "list" -d 'List the maintainers'
complete -c swarm -n "__fish_swarm_using_subcommand maintainers; and not __fish_seen_subcommand_from add remove list help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c swarm -n "__fish_swarm_using_subcommand maintainers; and __fish_seen_subcommand_from add" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand maintainers; and __fish_seen_subcommand_from remove" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand maintainers; and __fish_seen_subcommand_from list" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand maintainers; and __fish_seen_subcommand_from help" -f -a "add" -d 'Authorize a key (root key only), optionally limited to ref globs'
complete -c swarm -n "__fish_swarm_using_subcommand maintainers; and __fish_seen_subcommand_from help" -f -a "remove" -d 'Remove a maintainer (root key only)'
complete -c swarm -n "__fish_swarm_using_subcommand maintainers; and __fish_seen_subcommand_from help" -f -a "list" -d 'List the maintainers'
complete -c swarm -n "__fish_swarm_using_subcommand maintainers; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c swarm -n "__fish_swarm_using_subcommand accept" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand key; and not __fish_seen_subcommand_from recovery rotate new id backup help" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand key; and not __fish_seen_subcommand_from recovery rotate new id backup help" -f -a "recovery" -d 'Create a recovery key, name it in the repo, and save it to FILE (keep it offline)'
complete -c swarm -n "__fish_swarm_using_subcommand key; and not __fish_seen_subcommand_from recovery rotate new id backup help" -f -a "rotate" -d 'Move the root to a new key, authorized by the recovery key file'
complete -c swarm -n "__fish_swarm_using_subcommand key; and not __fish_seen_subcommand_from recovery rotate new id backup help" -f -a "new" -d 'Create this machine\'s own key for a repo, to be authorized by the root'
complete -c swarm -n "__fish_swarm_using_subcommand key; and not __fish_seen_subcommand_from recovery rotate new id backup help" -f -a "id" -d 'Print the key ID of this machine\'s key for a repo'
complete -c swarm -n "__fish_swarm_using_subcommand key; and not __fish_seen_subcommand_from recovery rotate new id backup help" -f -a "backup" -d 'Copy a repo signing key to FILE with mode 0600'
complete -c swarm -n "__fish_swarm_using_subcommand key; and not __fish_seen_subcommand_from recovery rotate new id backup help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c swarm -n "__fish_swarm_using_subcommand key; and __fish_seen_subcommand_from recovery" -s o -l output -r -F
complete -c swarm -n "__fish_swarm_using_subcommand key; and __fish_seen_subcommand_from recovery" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand key; and __fish_seen_subcommand_from rotate" -l recovery -r -F
complete -c swarm -n "__fish_swarm_using_subcommand key; and __fish_seen_subcommand_from rotate" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand key; and __fish_seen_subcommand_from new" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand key; and __fish_seen_subcommand_from id" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand key; and __fish_seen_subcommand_from backup" -s o -l output -r -F
complete -c swarm -n "__fish_swarm_using_subcommand key; and __fish_seen_subcommand_from backup" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand key; and __fish_seen_subcommand_from help" -f -a "recovery" -d 'Create a recovery key, name it in the repo, and save it to FILE (keep it offline)'
complete -c swarm -n "__fish_swarm_using_subcommand key; and __fish_seen_subcommand_from help" -f -a "rotate" -d 'Move the root to a new key, authorized by the recovery key file'
complete -c swarm -n "__fish_swarm_using_subcommand key; and __fish_seen_subcommand_from help" -f -a "new" -d 'Create this machine\'s own key for a repo, to be authorized by the root'
complete -c swarm -n "__fish_swarm_using_subcommand key; and __fish_seen_subcommand_from help" -f -a "id" -d 'Print the key ID of this machine\'s key for a repo'
complete -c swarm -n "__fish_swarm_using_subcommand key; and __fish_seen_subcommand_from help" -f -a "backup" -d 'Copy a repo signing key to FILE with mode 0600'
complete -c swarm -n "__fish_swarm_using_subcommand key; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c swarm -n "__fish_swarm_using_subcommand completions" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and not __fish_seen_subcommand_from stop help" -l foreground -d 'Stay in the foreground'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and not __fish_seen_subcommand_from stop help" -l detach -d 'Run as an already-detached process (used by autostart)'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and not __fish_seen_subcommand_from stop help" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and not __fish_seen_subcommand_from stop help" -f -a "stop" -d 'Stop the running daemon'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and not __fish_seen_subcommand_from stop help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and __fish_seen_subcommand_from stop" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and __fish_seen_subcommand_from help" -f -a "stop" -d 'Stop the running daemon'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status seed unseed gc fork export mirror import maintainers accept key completions daemon help" -f -a "init" -d 'Create a repo key and publish the first manifest'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status seed unseed gc fork export mirror import maintainers accept key completions daemon help" -f -a "link" -d 'Print the link for a repo'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status seed unseed gc fork export mirror import maintainers accept key completions daemon help" -f -a "status" -d 'Show repos, sequence numbers and conflicts'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status seed unseed gc fork export mirror import maintainers accept key completions daemon help" -f -a "seed" -d 'Sync a repo and keep serving it'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status seed unseed gc fork export mirror import maintainers accept key completions daemon help" -f -a "unseed" -d 'Stop serving a repo but keep its store'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status seed unseed gc fork export mirror import maintainers accept key completions daemon help" -f -a "gc" -d 'Repack a repo\'s store and drop unreachable objects, when idle (ST-7)'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status seed unseed gc fork export mirror import maintainers accept key completions daemon help" -f -a "fork" -d 'Create an independent repo from another one'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status seed unseed gc fork export mirror import maintainers accept key completions daemon help" -f -a "export" -d 'Write a repo and its signed state to a bundle file for offline transfer'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status seed unseed gc fork export mirror import maintainers accept key completions daemon help" -f -a "mirror" -d 'Publish a repo for static mirrors (plain file hosting)'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status seed unseed gc fork export mirror import maintainers accept key completions daemon help" -f -a "import" -d 'Read a bundle file, making its repo available offline'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status seed unseed gc fork export mirror import maintainers accept key completions daemon help" -f -a "maintainers" -d 'Add or remove maintainers, who may publish without the root key'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status seed unseed gc fork export mirror import maintainers accept key completions daemon help" -f -a "accept" -d 'Settle a ref conflict here by taking one signer\'s value'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status seed unseed gc fork export mirror import maintainers accept key completions daemon help" -f -a "key" -d 'Key management'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status seed unseed gc fork export mirror import maintainers accept key completions daemon help" -f -a "completions" -d 'Print a shell completion script (bash, zsh, fish, elvish, powershell)'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status seed unseed gc fork export mirror import maintainers accept key completions daemon help" -f -a "daemon" -d 'Run, start or stop the daemon'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status seed unseed gc fork export mirror import maintainers accept key completions daemon help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from mirror" -f -a "export" -d 'Write DIR/<repo-id>/repo.swarmbundle; serve DIR with any web server'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from maintainers" -f -a "add" -d 'Authorize a key (root key only), optionally limited to ref globs'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from maintainers" -f -a "remove" -d 'Remove a maintainer (root key only)'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from maintainers" -f -a "list" -d 'List the maintainers'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from key" -f -a "recovery" -d 'Create a recovery key, name it in the repo, and save it to FILE (keep it offline)'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from key" -f -a "rotate" -d 'Move the root to a new key, authorized by the recovery key file'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from key" -f -a "new" -d 'Create this machine\'s own key for a repo, to be authorized by the root'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from key" -f -a "id" -d 'Print the key ID of this machine\'s key for a repo'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from key" -f -a "backup" -d 'Copy a repo signing key to FILE with mode 0600'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from daemon" -f -a "stop" -d 'Stop the running daemon'
