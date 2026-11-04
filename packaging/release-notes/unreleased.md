<!-- User-visible changes since the last release, drafted by each pull request. See packaging/RELEASING.md. -->

## New

- **Refresh a single feed or folder** from its right-click menu, for instance to retry a feed marked unavailable without refreshing everything. Adding or editing a feed now refreshes only that feed.

## Changed

- **No more "Manage library" window.** Feeds and folders are organized right in the sidebar, as in NetNewsWire: drag a feed or folder onto a folder to move it inside, or onto a smart view to move it to the top level. The **+** button in the header adds a feed (Ctrl+N) or a folder (Ctrl+Shift+N) to the folder you are viewing, and a folder's right-click menu can add them inside it. **Edit…** and **Remove…** stay in the right-click menu. Ctrl+L no longer does anything.
- **Redesigned forms.** Adding and editing feeds and folders, choosing among a site's feeds, and Settings use wider dialogs with grouped rows, Cancel and Save in the header, and Enter to save. A mistake, such as a folder name already in use, shows in the form instead of closing it.
- **No status bar.** Refresh progress shows as a spinner in the refresh button, whose tooltip tells how far it got and when it last finished, and as a thin line along the top of the window. Page buttons sit beside the article count and only appear when a view has more than one page.
