using System.Windows.Input;

namespace InputWeave.Support;

/// <summary>A command that runs an action, enabled while <c>canExecute</c> says so.</summary>
internal sealed class RelayCommand(Action execute, Func<bool>? canExecute = null) : ICommand
{
    public event EventHandler? CanExecuteChanged;

    public bool CanExecute(object? parameter) => canExecute?.Invoke() ?? true;

    public void Execute(object? parameter)
    {
        if (CanExecute(parameter))
        {
            execute();
        }
    }

    /// <summary>Tells the controls bound to it to ask again; call on the UI thread.</summary>
    public void Refresh() => CanExecuteChanged?.Invoke(this, EventArgs.Empty);
}
