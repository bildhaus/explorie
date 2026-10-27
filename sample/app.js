/**
 * Main application script for explorie
 * This demonstrates a JavaScript file with custom fields
 */

// Theme handling - supports hot swapping CSS variables
class ThemeManager {
  constructor() {
    this.currentTheme = 'dark';
    this.listeners = [];
  }

  /**
   * Apply a theme by name
   * @param {string} themeName - The name of the theme to apply
   */
  applyTheme(themeName) {
    // Load theme from CSS file
    const link = document.getElementById('theme-stylesheet');
    if (!link) {
      const newLink = document.createElement('link');
      newLink.id = 'theme-stylesheet';
      newLink.rel = 'stylesheet';
      newLink.href = `./themes/${themeName}.css`;
      document.head.appendChild(newLink);
    } else {
      link.href = `./themes/${themeName}.css`;
    }

    this.currentTheme = themeName;

    // Notify listeners
    this.listeners.forEach((listener) => listener(themeName));

    // Save preference
    localStorage.setItem('explorie-theme', themeName);

    console.log(`Theme applied: ${themeName}`);
  }

  /**
   * Register a listener for theme changes
   * @param {Function} callback - Function to call when theme changes
   */
  onChange(callback) {
    this.listeners.push(callback);
  }

  /**
   * Get the current theme name
   * @returns {string} The current theme name
   */
  getCurrentTheme() {
    return this.currentTheme;
  }

  /**
   * Toggle between light and dark themes
   */
  toggleDarkMode() {
    const newTheme = this.currentTheme === 'light' ? 'dark' : 'light';
    this.applyTheme(newTheme);
  }
}

// File utilities for the app
class FileUtils {
  /**
   * Get file extension from path
   * @param {string} path - The file path
   * @returns {string} The file extension
   */
  static getExtension(path) {
    return path.split('.').pop().toLowerCase();
  }

  /**
   * Format file size for display
   * @param {number} bytes - Size in bytes
   * @returns {string} Formatted size string
   */
  static formatSize(bytes) {
    if (bytes === 0) return '0 Bytes';
    const k = 1024;
    const sizes = ['Bytes', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i];
  }

  /**
   * Check if a file is a directory
   * @param {Object} file - File object
   * @returns {boolean} True if directory
   */
  static isDirectory(file) {
    // In most platforms, directories have size 0 and no extension
    return file.size === 0 && !file.path.includes('.');
  }
}

// Initialize app
document.addEventListener('DOMContentLoaded', () => {
  console.log('explorie app initializing...');

  // Create theme manager
  const themeManager = new ThemeManager();

  // Load saved theme or use default
  const savedTheme = localStorage.getItem('explorie-theme');
  themeManager.applyTheme(savedTheme || 'dark');

  // Set up theme toggle button
  const themeToggle = document.getElementById('theme-toggle');
  if (themeToggle) {
    themeToggle.addEventListener('click', () => {
      themeManager.toggleDarkMode();
    });
  }

  console.log('explorie app initialized');
});
