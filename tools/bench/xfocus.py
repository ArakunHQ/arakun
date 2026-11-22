#!/usr/bin/env python3
"""Gives the X input focus to the top-level windows of a display (argv[1]), so
a window on Xvfb, where no window manager hands out the focus, reads itself as
active. Uses libX11 through ctypes: no xdotool needed."""

import ctypes
import sys

X = ctypes.cdll.LoadLibrary("libX11.so.6")
Win = ctypes.c_ulong
ERROR_HANDLER = ctypes.CFUNCTYPE(ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p)


def main():
    X.XOpenDisplay.restype = ctypes.c_void_p
    X.XDefaultRootWindow.restype = Win
    X.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
    X.XQueryTree.argtypes = [ctypes.c_void_p, Win, ctypes.POINTER(Win), ctypes.POINTER(Win),
                             ctypes.POINTER(ctypes.POINTER(Win)), ctypes.POINTER(ctypes.c_uint)]
    X.XSetInputFocus.argtypes = [ctypes.c_void_p, Win, ctypes.c_int, ctypes.c_ulong]
    X.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
    X.XCloseDisplay.argtypes = [ctypes.c_void_p]
    # Unmapped windows refuse the focus with BadMatch; Xlib's default handler
    # would exit on the first one.
    ignore = ERROR_HANDLER(lambda _d, _e: 0)
    X.XSetErrorHandler(ignore)
    d = X.XOpenDisplay(sys.argv[1].encode())
    if not d:
        sys.exit(f"cannot open display {sys.argv[1]}")
    root, parent, kids, n = Win(), Win(), ctypes.POINTER(Win)(), ctypes.c_uint()
    X.XQueryTree(d, X.XDefaultRootWindow(d), ctypes.byref(root), ctypes.byref(parent),
                 ctypes.byref(kids), ctypes.byref(n))
    for i in range(n.value):
        X.XSetInputFocus(d, kids[i], 1, 0)  # RevertToPointerRoot, CurrentTime
        X.XSync(d, 0)
    X.XCloseDisplay(d)


if __name__ == "__main__":
    main()
