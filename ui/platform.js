// Loaded as a classic script in <head>, so it runs before the first paint:
// the space for the macOS window buttons is right in the very first frame.
if (navigator.userAgent.includes("Mac")) document.documentElement.classList.add("mac");
