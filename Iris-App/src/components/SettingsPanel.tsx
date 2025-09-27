// filepath: d:\Iris_for_Godot\Iris-App\src\components\SettingsPanel.tsx
import React from "react";

type Props = {
  isOpen: boolean;
  onClose: () => void;
  values: Record<string, any>;
  onChange: (values: Record<string, any>) => void;
};

const SettingsPanel: React.FC<Props> = ({ isOpen, onClose, values, onChange }) => {
  if (!isOpen) return null;
  return (
    <div className="settings-panel">
      <div>
        <h3>Settings</h3>
        <div>
          <label>Model:</label>
          <select value={values.model || ""} onChange={e => onChange({ ...values, model: e.target.value })}>
            <option value="">Select model</option>
            <option value="iris-coder:latest">iris-coder:latest</option>
            <option value="iris-organizer:latest">iris-organizer:latest</option>
          </select>
        </div>
        <div>
          <label>
            <input
              type="checkbox"
              checked={!!values.experimental}
              onChange={e => onChange({ ...values, experimental: e.target.checked })}
            />
            Enable experimental features
          </label>
        </div>
        <button onClick={onClose}>Close</button>
      </div>
    </div>
  );
};

export default SettingsPanel;