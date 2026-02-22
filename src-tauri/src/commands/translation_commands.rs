use crate::model::settings::TranslationProvider;

#[tauri::command]
pub async fn translate_text(
    provider: TranslationProvider,
    api_key: String,
    text: String,
    target_lang: String,
    api_url: Option<String>,
) -> Result<String, String> {
    crate::i18n::translation::TranslationService::translate(
        &provider,
        &api_key,
        &text,
        &target_lang,
        api_url.as_deref(),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn generate_language_file(
    provider: TranslationProvider,
    api_key: String,
    target_lang: String,
    api_url: Option<String>,
) -> Result<String, String> {
    crate::i18n::translation::TranslationService::generate_language_file(
        &provider,
        &api_key,
        &target_lang,
        api_url.as_deref(),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn test_api_connection(
    provider: TranslationProvider,
    api_key: String,
    api_url: Option<String>,
) -> Result<bool, String> {
    crate::i18n::translation::TranslationService::test_connection(
        &provider,
        &api_key,
        api_url.as_deref(),
    )
    .await
    .map_err(|e| e.to_string())
}
