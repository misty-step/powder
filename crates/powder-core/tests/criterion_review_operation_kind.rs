use powder_core::OperationKind;

#[test]
fn criterion_review_operation_kind_round_trips() {
    assert_eq!(OperationKind::CriterionReview.as_str(), "criterion_review");
    assert_eq!(
        OperationKind::parse("criterion_review"),
        Some(OperationKind::CriterionReview)
    );
}
