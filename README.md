# StudyFlow (Rust) – application de bureau

Même fonctionnalités que la version web : import de cours (PDF, TXT, DOCX, ODT),
emploi du temps avec semaines A/B, export/import JSON (compatible avec la version web).

## Obtenir l'application sans rien installer
1. Crée un dépôt GitHub et envoie-y tout ce dossier (dont `.github`).
2. Onglet **Actions** > **Build StudyFlow** > **Run workflow**.
3. Télécharge l'artefact de ton système (Windows / macOS / Linux) et lance-le.

## Compiler soi-même
Installer Rust (https://rustup.rs) puis : `cargo run --release`
Les données sont enregistrées automatiquement dans le dossier de données de l'utilisateur
(`StudyFlow/studyflow.json`).
