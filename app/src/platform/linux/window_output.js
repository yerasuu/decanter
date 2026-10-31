// Written by decanter for this session: KWin opens decanter's windows on
// one output, since a Wayland client can't choose one for itself.
const OUTPUT = "@OUTPUT@";

function place(window) {
    if (window.resourceClass !== "decanter" || !window.normalWindow) {
        return;
    }
    const output = workspace.screens.find((screen) => screen.name === OUTPUT);
    if (output && window.output !== output) {
        workspace.sendClientToScreen(window, output);
    }
}

workspace.windowList().forEach(place);
workspace.windowAdded.connect(place);
