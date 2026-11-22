export function microphoneOptions(status) {
  const devices = status.devices || [];
  const selected = status.selected ?? null;
  const defaultDevice = devices.find(device => device.is_default);
  const options = [{ name: null, label: `System default${defaultDevice ? ` (${defaultDevice.name})` : ' (unavailable)'}` },
    ...devices.map(device => ({ name: device.name, label: device.name }))];
  if (selected && !devices.some(device => device.name === selected)) {
    options.push({ name: selected, label: `${selected} (unavailable)`, unavailable: true });
  }
  // Settings may change during capture; the saved choice applies next time.
  return options.map(option => ({ ...option, selected: option.name === selected }));
}
