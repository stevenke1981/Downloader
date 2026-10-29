use super::*;

#[test]
fn sync_model_matches_items_for_growth_change_and_shrink() {
    let model = VecModel::from(vec![1, 2, 3]);
    sync_model(&model, vec![1, 9, 3, 4]);
    assert_eq!(model.iter().collect::<Vec<_>>(), vec![1, 9, 3, 4]);
    sync_model(&model, vec![7]);
    assert_eq!(model.iter().collect::<Vec<_>>(), vec![7]);
    sync_model(&model, Vec::new());
    assert_eq!(model.row_count(), 0);
}
