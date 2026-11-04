<!-- User-visible changes since the last release, drafted by each pull request. See packaging/RELEASING.md. -->

## New

- **A right-click menu on articles,** as in NetNewsWire: mark as read or unread, star or unstar, mark the articles above or below as read (Ctrl+Z undoes it), open in the browser, copy the link, and go to the article's feed. The Menu key or Shift+F10 opens it for the selected article.
- **Refresh a single feed or folder** from its right-click menu, for instance to retry a feed marked unavailable without refreshing everything. Adding or editing a feed now refreshes only that feed.

## Changed

- **No more "Manage library" window.** Feeds and folders are organized right in the sidebar, as in NetNewsWire: drag a feed or folder onto a folder to move it inside, or onto a smart view to move it to the top level. The **+** button in the header adds a feed (Ctrl+N) or a folder (Ctrl+Shift+N) to the folder you are viewing, and a folder's right-click menu can add them inside it. **Edit…** and **Remove…** stay in the right-click menu. Ctrl+L no longer does anything.
- **Redesigned dialogs.** Adding and editing feeds and folders, choosing among a site's feeds, Settings, Content filters and Keyboard shortcuts have a new look in omafeed's own style: a clear title, labeled fields, Cancel and Save at the bottom, and Enter to save. A mistake, such as a folder name already in use, shows in the form instead of closing it.
- **No status bar.** Refresh progress shows as a spinner in the refresh button, whose tooltip tells how far it got and when it last finished, and as a thin line along the top of the window. Page buttons sit beside the article count and only appear when a view has more than one page.
- **The sidebar no longer has a "LIBRARY" heading,** and folders have a folder icon, with their expander at the end of the row.
- **A quieter header and a lowercase name.** The title is a small "omafeed" wordmark, and the app is called omafeed everywhere, including the launcher.
