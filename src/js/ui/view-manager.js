export const ViewManager = {
  closeAll() {
    document.body.classList.remove('detail-open', 'settings-open', 'prompts-open', 'pipelines-open');
  },

  showRecordings() {
    this.closeAll();
    this.updateSidebar('recordings');
  },

  showDetail() {
    this.closeAll();
    document.body.classList.add('detail-open');
    this.updateSidebar('recordings');
  },

  showSettings() {
    this.closeAll();
    document.body.classList.add('settings-open');
    this.updateSidebar('settings');
  },

  showPrompts() {
    this.closeAll();
    document.body.classList.add('prompts-open');
    this.updateSidebar('prompts');
  },

  showPipelines() {
    this.closeAll();
    document.body.classList.add('pipelines-open');
    this.updateSidebar('pipelines');
  },

  updateSidebar(view) {
    document.querySelectorAll('.sidebar-nav-item').forEach(item => {
      item.classList.toggle('active', item.dataset.view === view);
    });
  }
};
