-- Lays out the DMG's window: the app on the left screen of background.tiff
-- (drawn by apps/macos/scripts/make-brand-assets.swift), the Applications
-- alias on the right one. package.sh runs it on the read-write image it
-- has just filled, and Finder saves the layout in the volume's .DS_Store:
--
--   osascript layout.applescript <volume name>
--
-- Positions are icon centers in the window's content area, whose top left
-- corner is the picture's. The bounds are the whole window, title bar
-- included, and title bars differ between macOS versions. Allowing 28
-- points for it keeps the picture filling the window down to a 28-point
-- title bar; under the 32-point one of macOS 27 only the picture's bottom
-- 4 points, plain canvas, are hidden.

on run argv
	set volumeName to item 1 of argv
	tell application "Finder"
		set theDisk to disk volumeName
		open theDisk
		set theWindow to container window of theDisk
		set current view of theWindow to icon view
		set toolbar visible of theWindow to false
		set statusbar visible of theWindow to false
		set pathbar visible of theWindow to false
		set sidebar width of theWindow to 0
		set bounds of theWindow to {200, 120, 200 + 600, 120 + 400 + 28}
		set theOptions to icon view options of theWindow
		set arrangement of theOptions to not arranged
		set icon size of theOptions to 128
		set text size of theOptions to 13
		set label position of theOptions to bottom
		set shows item info of theOptions to false
		set shows icon preview of theOptions to false
		set background picture of theOptions to file ".background:background.tiff" of theDisk
		set position of item "InputWeave.app" of theDisk to {150, 185}
		set position of item "Applications" of theDisk to {450, 185}
		-- Finder applies the view options once the window opens again.
		close theWindow
		open theDisk
		update theDisk without registering applications
		delay 1
		close container window of theDisk
	end tell
end run
