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
complete -c swarm -n "__fish_swarm_needs_command" -f -a "status" -d 'Show the daemon, repos, relays and peers'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "inspect" -d 'Show a repo\'s name, branches and forks without downloading Git objects'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "about" -d 'Show or set a repo\'s name and description (root or maintainer key)'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "discover-relays" -d 'Find public relay servers through the Mainline DHT and relay gossip'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "seed" -d 'Sync a repo and keep serving it'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "unseed" -d 'Stop serving a repo but keep its store'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "gc" -d 'Repack a repo\'s store and drop unreachable objects, when idle (ST-7)'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "purge" -d 'Delete a repo\'s store and manifests, and stop serving it'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "mr" -d 'Merge requests: propose changes, discuss, review and merge them'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "fork" -d 'Create an independent repo from another one'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "export" -d 'Write a repo and its signed state to a bundle file for offline transfer'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "mirror" -d 'Publish a repo for static mirrors (plain file hosting)'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "import" -d 'Read a bundle file, making its repo available offline'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "maintainers" -d 'Add or remove maintainers, who may publish without the root key'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "accept" -d 'Settle a ref conflict here by taking one signer\'s value'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "key" -d 'Key management'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "completions" -d 'Print a shell completion script (bash, zsh, fish, elvish, powershell)'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "relay" -d 'Run a relay server for peers that cannot accept connections'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "daemon" -d 'Start, stop or restart the background daemon'
complete -c swarm -n "__fish_swarm_needs_command" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c swarm -n "__fish_swarm_using_subcommand init" -l object-format -d 'Object format for a newly created git repo' -r
complete -c swarm -n "__fish_swarm_using_subcommand init" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand init" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand link" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand link" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand status; and not __fish_seen_subcommand_from repos relays peers help" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand status; and not __fish_seen_subcommand_from repos relays peers help" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand status; and not __fish_seen_subcommand_from repos relays peers help" -a "repos" -d 'Repositories: sequence numbers, seeders and conflicts'
complete -c swarm -n "__fish_swarm_using_subcommand status; and not __fish_seen_subcommand_from repos relays peers help" -a "relays" -d 'Relay registrations'
complete -c swarm -n "__fish_swarm_using_subcommand status; and not __fish_seen_subcommand_from repos relays peers help" -a "peers" -d 'Peers known for each repo, and which are connected'
complete -c swarm -n "__fish_swarm_using_subcommand status; and not __fish_seen_subcommand_from repos relays peers help" -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c swarm -n "__fish_swarm_using_subcommand status; and __fish_seen_subcommand_from repos" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand status; and __fish_seen_subcommand_from repos" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand status; and __fish_seen_subcommand_from relays" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand status; and __fish_seen_subcommand_from relays" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand status; and __fish_seen_subcommand_from peers" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand status; and __fish_seen_subcommand_from peers" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand status; and __fish_seen_subcommand_from help" -f -a "repos" -d 'Repositories: sequence numbers, seeders and conflicts'
complete -c swarm -n "__fish_swarm_using_subcommand status; and __fish_seen_subcommand_from help" -f -a "relays" -d 'Relay registrations'
complete -c swarm -n "__fish_swarm_using_subcommand status; and __fish_seen_subcommand_from help" -f -a "peers" -d 'Peers known for each repo, and which are connected'
complete -c swarm -n "__fish_swarm_using_subcommand status; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c swarm -n "__fish_swarm_using_subcommand inspect" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand inspect" -l progress -d 'Stream lookup progress as JSON lines'
complete -c swarm -n "__fish_swarm_using_subcommand inspect" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand about" -l name -d 'New name, at most 64 bytes. "" removes it' -r
complete -c swarm -n "__fish_swarm_using_subcommand about" -l description -d 'New one-line description, at most 512 bytes. "" removes it' -r
complete -c swarm -n "__fish_swarm_using_subcommand about" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand about" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand discover-relays" -l json -d 'Print a JSON object containing protocol-verified public relays'
complete -c swarm -n "__fish_swarm_using_subcommand discover-relays" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand seed" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand seed" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand unseed" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand unseed" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand gc" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand gc" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand purge" -l yes -d 'Do not ask for confirmation. The signing key is kept'
complete -c swarm -n "__fish_swarm_using_subcommand purge" -l delete-key -d 'Also delete the signing key. It cannot be recreated'
complete -c swarm -n "__fish_swarm_using_subcommand purge" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand purge" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and not __fish_seen_subcommand_from list show create comment review checkout diff merge close reopen help" -s R -l repo -d 'A repo ID or link; by default the swarm remote of this clone' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and not __fish_seen_subcommand_from list show create comment review checkout diff merge close reopen help" -l offline -d 'Use what is already here instead of syncing first'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and not __fish_seen_subcommand_from list show create comment review checkout diff merge close reopen help" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and not __fish_seen_subcommand_from list show create comment review checkout diff merge close reopen help" -f -a "list" -d 'List open merge requests'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and not __fish_seen_subcommand_from list show create comment review checkout diff merge close reopen help" -f -a "show" -d 'Show one merge request, its commits and conversation'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and not __fish_seen_subcommand_from list show create comment review checkout diff merge close reopen help" -f -a "create" -d 'Propose a branch; run it again to add your new commits'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and not __fish_seen_subcommand_from list show create comment review checkout diff merge close reopen help" -f -a "comment" -d 'Comment on a merge request, a file in it, or another comment'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and not __fish_seen_subcommand_from list show create comment review checkout diff merge close reopen help" -f -a "review" -d 'Approve a merge request or ask for changes'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and not __fish_seen_subcommand_from list show create comment review checkout diff merge close reopen help" -f -a "checkout" -d 'Check out the proposed commits as branch mr/ID'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and not __fish_seen_subcommand_from list show create comment review checkout diff merge close reopen help" -f -a "diff" -d 'Show the proposed changes'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and not __fish_seen_subcommand_from list show create comment review checkout diff merge close reopen help" -f -a "merge" -d 'Merge into the target branch and publish (root or maintainer key)'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and not __fish_seen_subcommand_from list show create comment review checkout diff merge close reopen help" -f -a "close" -d 'Close without merging (its author or a maintainer)'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and not __fish_seen_subcommand_from list show create comment review checkout diff merge close reopen help" -f -a "reopen" -d 'Reopen a closed merge request'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and not __fish_seen_subcommand_from list show create comment review checkout diff merge close reopen help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from list" -s R -l repo -d 'A repo ID or link; by default the swarm remote of this clone' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from list" -l all -d 'Include merged and closed ones'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from list" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from list" -l offline -d 'Use what is already here instead of syncing first'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from list" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from show" -s R -l repo -d 'A repo ID or link; by default the swarm remote of this clone' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from show" -l json -d 'Print one JSON object on stdout'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from show" -l offline -d 'Use what is already here instead of syncing first'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from show" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from create" -l base -d 'Branch to merge into (default: the repo\'s default branch)' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from create" -l title -d 'Title (default: taken from the commits)' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from create" -s m -l message -d 'Description' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from create" -s R -l repo -d 'A repo ID or link; by default the swarm remote of this clone' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from create" -l offline -d 'Use what is already here instead of syncing first'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from create" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from comment" -s m -l message -d 'The comment; without it your git editor opens' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from comment" -l file -d 'File the comment is about' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from comment" -l line -d 'Line of that file' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from comment" -l reply-to -d 'Comment being answered, by the ID shown before it' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from comment" -s R -l repo -d 'A repo ID or link; by default the swarm remote of this clone' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from comment" -l offline -d 'Use what is already here instead of syncing first'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from comment" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from review" -s m -l message -d 'What you think' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from review" -s R -l repo -d 'A repo ID or link; by default the swarm remote of this clone' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from review" -l approve -d 'Approve the commits as they are now'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from review" -l request-changes -d 'Ask for changes; say which with -m'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from review" -l offline -d 'Use what is already here instead of syncing first'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from review" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from checkout" -s R -l repo -d 'A repo ID or link; by default the swarm remote of this clone' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from checkout" -l offline -d 'Use what is already here instead of syncing first'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from checkout" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from diff" -s R -l repo -d 'A repo ID or link; by default the swarm remote of this clone' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from diff" -l offline -d 'Use what is already here instead of syncing first'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from diff" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from merge" -s R -l repo -d 'A repo ID or link; by default the swarm remote of this clone' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from merge" -l no-push -d 'Make the merge commit but do not push it'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from merge" -l offline -d 'Use what is already here instead of syncing first'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from merge" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from close" -s m -l message -d 'Why' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from close" -s R -l repo -d 'A repo ID or link; by default the swarm remote of this clone' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from close" -l offline -d 'Use what is already here instead of syncing first'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from close" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from reopen" -s R -l repo -d 'A repo ID or link; by default the swarm remote of this clone' -r
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from reopen" -l offline -d 'Use what is already here instead of syncing first'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from reopen" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from help" -f -a "list" -d 'List open merge requests'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from help" -f -a "show" -d 'Show one merge request, its commits and conversation'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from help" -f -a "create" -d 'Propose a branch; run it again to add your new commits'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from help" -f -a "comment" -d 'Comment on a merge request, a file in it, or another comment'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from help" -f -a "review" -d 'Approve a merge request or ask for changes'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from help" -f -a "checkout" -d 'Check out the proposed commits as branch mr/ID'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from help" -f -a "diff" -d 'Show the proposed changes'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from help" -f -a "merge" -d 'Merge into the target branch and publish (root or maintainer key)'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from help" -f -a "close" -d 'Close without merging (its author or a maintainer)'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from help" -f -a "reopen" -d 'Reopen a closed merge request'
complete -c swarm -n "__fish_swarm_using_subcommand mr; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
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
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l listen -d 'Address to listen on' -r
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l name -d 'Name clients dial this relay by, as host:port. Repeatable. A listener\'s proof is bound to it. Without any, proofs are not bound to a name' -r
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l allow-node -d 'Let only this node register. Repeatable. Keeps the relay out of the relay network' -r
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l password-file -d 'Require the secret in this file to register or dial. Clients send it only to an address that includes the relay\'s key, so this needs --key-file. Keeps the relay out of the relay network' -r -F
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l key-file -d 'Keep the relay\'s key in this file, created on first use, so that `host:port#key` addresses keep working across restarts' -r -F
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l metrics -d 'Serve counters over HTTP on this address (off by default)' -r
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l max-listeners -d 'Most registered listeners' -r
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l max-splices -d 'Most simultaneous splices' -r
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l listeners-per-ip -d 'Most nodes registered from one address' -r
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l splices-per-ip -d 'Most splices one dialing address may have open' -r
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l connects-per-minute -d 'Most connect attempts from one address in a minute' -r
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l requests-per-minute -d 'Most info and lookup requests from one address in a minute' -r
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l splice-kib-per-sec -d 'Rate of one splice in KiB per second, 0 for no limit' -r
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l node-daily-mib -d 'Most bytes carried for one node in a day, in MiB' -r
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l idle-secs -d 'Close a splice after this many idle seconds' -r
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l round-secs -d 'Seconds between relay network rounds (gossip)' -r
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l no-network -d 'Stay out of the relay network: no DHT announce and no gossip'
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l public -d 'This relay is reachable from the internet: announce it without first asking another relay to dial back'
complete -c swarm -n "__fish_swarm_using_subcommand relay" -l allow-private -d 'Accept relays on loopback and private addresses, for a closed network'
complete -c swarm -n "__fish_swarm_using_subcommand relay" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and not __fish_seen_subcommand_from start stop restart help" -l foreground -d 'Run the daemon in this terminal (for service managers)'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and not __fish_seen_subcommand_from start stop restart help" -l detach -d 'Run as an already-detached process (used by autostart)'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and not __fish_seen_subcommand_from start stop restart help" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and not __fish_seen_subcommand_from start stop restart help" -f -a "start" -d 'Start the daemon in the background'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and not __fish_seen_subcommand_from start stop restart help" -f -a "stop" -d 'Stop the running daemon'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and not __fish_seen_subcommand_from start stop restart help" -f -a "restart" -d 'Stop the daemon if it is running, then start it'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and not __fish_seen_subcommand_from start stop restart help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and __fish_seen_subcommand_from start" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and __fish_seen_subcommand_from stop" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and __fish_seen_subcommand_from restart" -s h -l help -d 'Print help'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and __fish_seen_subcommand_from help" -f -a "start" -d 'Start the daemon in the background'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and __fish_seen_subcommand_from help" -f -a "stop" -d 'Stop the running daemon'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and __fish_seen_subcommand_from help" -f -a "restart" -d 'Stop the daemon if it is running, then start it'
complete -c swarm -n "__fish_swarm_using_subcommand daemon; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "init" -d 'Create a repo key and publish the first manifest'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "link" -d 'Print the link for a repo'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "status" -d 'Show the daemon, repos, relays and peers'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "inspect" -d 'Show a repo\'s name, branches and forks without downloading Git objects'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "about" -d 'Show or set a repo\'s name and description (root or maintainer key)'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "discover-relays" -d 'Find public relay servers through the Mainline DHT and relay gossip'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "seed" -d 'Sync a repo and keep serving it'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "unseed" -d 'Stop serving a repo but keep its store'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "gc" -d 'Repack a repo\'s store and drop unreachable objects, when idle (ST-7)'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "purge" -d 'Delete a repo\'s store and manifests, and stop serving it'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "mr" -d 'Merge requests: propose changes, discuss, review and merge them'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "fork" -d 'Create an independent repo from another one'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "export" -d 'Write a repo and its signed state to a bundle file for offline transfer'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "mirror" -d 'Publish a repo for static mirrors (plain file hosting)'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "import" -d 'Read a bundle file, making its repo available offline'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "maintainers" -d 'Add or remove maintainers, who may publish without the root key'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "accept" -d 'Settle a ref conflict here by taking one signer\'s value'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "key" -d 'Key management'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "completions" -d 'Print a shell completion script (bash, zsh, fish, elvish, powershell)'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "relay" -d 'Run a relay server for peers that cannot accept connections'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "daemon" -d 'Start, stop or restart the background daemon'
complete -c swarm -n "__fish_swarm_using_subcommand help; and not __fish_seen_subcommand_from init link status inspect about discover-relays seed unseed gc purge mr fork export mirror import maintainers accept key completions relay daemon help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from status" -f -a "repos" -d 'Repositories: sequence numbers, seeders and conflicts'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from status" -f -a "relays" -d 'Relay registrations'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from status" -f -a "peers" -d 'Peers known for each repo, and which are connected'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from mr" -f -a "list" -d 'List open merge requests'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from mr" -f -a "show" -d 'Show one merge request, its commits and conversation'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from mr" -f -a "create" -d 'Propose a branch; run it again to add your new commits'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from mr" -f -a "comment" -d 'Comment on a merge request, a file in it, or another comment'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from mr" -f -a "review" -d 'Approve a merge request or ask for changes'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from mr" -f -a "checkout" -d 'Check out the proposed commits as branch mr/ID'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from mr" -f -a "diff" -d 'Show the proposed changes'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from mr" -f -a "merge" -d 'Merge into the target branch and publish (root or maintainer key)'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from mr" -f -a "close" -d 'Close without merging (its author or a maintainer)'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from mr" -f -a "reopen" -d 'Reopen a closed merge request'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from mirror" -f -a "export" -d 'Write DIR/<repo-id>/repo.swarmbundle; serve DIR with any web server'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from maintainers" -f -a "add" -d 'Authorize a key (root key only), optionally limited to ref globs'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from maintainers" -f -a "remove" -d 'Remove a maintainer (root key only)'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from maintainers" -f -a "list" -d 'List the maintainers'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from key" -f -a "recovery" -d 'Create a recovery key, name it in the repo, and save it to FILE (keep it offline)'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from key" -f -a "rotate" -d 'Move the root to a new key, authorized by the recovery key file'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from key" -f -a "new" -d 'Create this machine\'s own key for a repo, to be authorized by the root'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from key" -f -a "id" -d 'Print the key ID of this machine\'s key for a repo'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from key" -f -a "backup" -d 'Copy a repo signing key to FILE with mode 0600'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from daemon" -f -a "start" -d 'Start the daemon in the background'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from daemon" -f -a "stop" -d 'Stop the running daemon'
complete -c swarm -n "__fish_swarm_using_subcommand help; and __fish_seen_subcommand_from daemon" -f -a "restart" -d 'Stop the daemon if it is running, then start it'
